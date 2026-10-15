//! Notification → event mapping, one case per kind, fed straight into the mapper.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use serde_json::{Value, json};
use staffy_agents::codex::Mapper;
use staffy_core::agent::{
    MAX_OUTPUT_BYTES, MessageRole, NoticeLevel, PermissionKind, PermissionOutcome, PlanStepStatus,
    ReasoningPart, ToolKind, ToolStatus,
};
use staffy_core::{AgentEventDraft, AgentEventKind};

const THREAD: &str = "thread-1";
const TURN: &str = "turn-1";

fn mapper() -> Mapper {
    Mapper::new(THREAD.to_owned())
}

fn only(mut drafts: Vec<AgentEventDraft>) -> AgentEventDraft {
    assert_eq!(drafts.len(), 1, "{drafts:?}");
    drafts.pop().unwrap()
}

fn notify(mapper: &mut Mapper, method: &str, mut params: Value) -> Vec<AgentEventDraft> {
    params["threadId"] = json!(THREAD);
    mapper.notification(method, params)
}

fn item(mapper: &mut Mapper, method: &str, item: Value) -> Vec<AgentEventDraft> {
    notify(
        mapper,
        method,
        json!({"turnId": TURN, "item": item, "startedAtMs": 1, "completedAtMs": 2}),
    )
}

#[test]
fn turn_lifecycle_uses_one_local_turn_id() {
    let mut m = mapper();
    let started = only(notify(
        &mut m,
        "turn/started",
        json!({"turn": {"id": TURN, "status": "inProgress"}}),
    ));
    assert_eq!(started.kind, AgentEventKind::TurnStarted);
    assert_eq!(started.provider.turn_id.as_deref(), Some(TURN));
    assert_eq!(started.provider.thread_id.as_deref(), Some(THREAD));
    let local = started.turn_id.expect("a local turn id");

    let done = only(notify(
        &mut m,
        "turn/completed",
        json!({"turn": {"id": TURN, "status": "completed", "durationMs": 42}}),
    ));
    assert_eq!(
        done.kind,
        AgentEventKind::TurnCompleted {
            duration_ms: Some(42)
        }
    );
    assert_eq!(done.turn_id, Some(local));
}

#[test]
fn turn_endings_map_to_completed_failed_and_cancelled() {
    let mut m = mapper();
    let cancelled = only(notify(
        &mut m,
        "turn/completed",
        json!({"turn": {"id": "t2", "status": "interrupted"}}),
    ));
    assert_eq!(cancelled.kind, AgentEventKind::TurnCancelled);
    let failed = only(notify(
        &mut m,
        "turn/completed",
        json!({"turn": {"id": "t3", "status": "failed",
            "error": {"message": "boom", "additionalDetails": "more"}}}),
    ));
    assert_eq!(
        failed.kind,
        AgentEventKind::TurnFailed {
            message: "boom".into(),
            details: Some("more".into())
        }
    );
    // A status this build does not know ends nothing.
    assert!(
        notify(
            &mut m,
            "turn/completed",
            json!({"turn": {"id": "t4", "status": "paused"}})
        )
        .is_empty()
    );
}

#[test]
fn backend_errors_are_summarised_and_kept_verbatim() {
    let mut m = mapper();
    let raw = r#"{"type":"error","status":400,"error":{"type":"invalid_request_error","message":"The model is not supported."}}"#;
    let failed = only(notify(
        &mut m,
        "turn/completed",
        json!({"turn": {"id": "t1", "status": "failed", "error": {"message": raw}}}),
    ));
    assert_eq!(
        failed.kind,
        AgentEventKind::TurnFailed {
            message: "The model is not supported.".into(),
            details: Some(raw.into())
        }
    );
    let notice = only(notify(
        &mut m,
        "error",
        json!({"turnId": "t1", "error": {"message": raw}, "willRetry": false}),
    ));
    assert_eq!(
        notice.kind,
        AgentEventKind::Notice {
            level: NoticeLevel::Error,
            message: "The model is not supported.".into()
        }
    );
}

#[test]
fn messages_stream_and_end_with_the_whole_text() {
    let mut m = mapper();
    let start = only(item(
        &mut m,
        "item/started",
        json!({"type": "agentMessage", "id": "m1", "text": "", "phase": "commentary"}),
    ));
    assert_eq!(
        start.kind,
        AgentEventKind::MessageStart {
            role: MessageRole::Assistant,
            phase: Some("commentary".into())
        }
    );
    let item_id = start.item_id.unwrap();
    let delta = only(notify(
        &mut m,
        "item/agentMessage/delta",
        json!({"turnId": TURN, "itemId": "m1", "delta": "Hel"}),
    ));
    assert_eq!(
        delta.kind,
        AgentEventKind::MessageDelta { text: "Hel".into() }
    );
    assert_eq!(
        delta.item_id,
        Some(item_id),
        "deltas share the message's local id"
    );
    assert!(delta.payload.is_none(), "streaming events carry no payload");
    let end = only(item(
        &mut m,
        "item/completed",
        json!({"type": "agentMessage", "id": "m1", "text": "Hello", "phase": "commentary"}),
    ));
    assert_eq!(
        end.kind,
        AgentEventKind::MessageEnd {
            role: MessageRole::Assistant,
            phase: Some("commentary".into()),
            text: "Hello".into()
        }
    );
    assert_eq!(end.item_id, Some(item_id));
}

#[test]
fn user_messages_join_their_text_parts() {
    let mut m = mapper();
    let end = only(item(
        &mut m,
        "item/completed",
        json!({"type": "userMessage", "id": "u1", "content": [
            {"type": "text", "text": "one"}, {"type": "image", "url": "x"}, {"type": "text", "text": "two"}]}),
    ));
    assert_eq!(
        end.kind,
        AgentEventKind::MessageEnd {
            role: MessageRole::User,
            phase: None,
            text: "one\ntwo".into()
        }
    );
}

#[test]
fn reasoning_deltas_name_their_part_and_the_end_keeps_both() {
    let mut m = mapper();
    assert_eq!(
        only(item(
            &mut m,
            "item/started",
            json!({"type": "reasoning", "id": "r1"})
        ))
        .kind,
        AgentEventKind::ReasoningStart
    );
    let summary = only(notify(
        &mut m,
        "item/reasoning/summaryTextDelta",
        json!({"turnId": TURN, "itemId": "r1", "delta": "s", "summaryIndex": 0}),
    ));
    assert_eq!(
        summary.kind,
        AgentEventKind::ReasoningDelta {
            part: ReasoningPart::Summary,
            text: "s".into()
        }
    );
    let content = only(notify(
        &mut m,
        "item/reasoning/textDelta",
        json!({"turnId": TURN, "itemId": "r1", "delta": "c", "contentIndex": 0}),
    ));
    assert_eq!(
        content.kind,
        AgentEventKind::ReasoningDelta {
            part: ReasoningPart::Content,
            text: "c".into()
        }
    );
    let end = only(item(
        &mut m,
        "item/completed",
        json!({"type": "reasoning", "id": "r1", "summary": ["s"], "content": ["c"]}),
    ));
    assert_eq!(
        end.kind,
        AgentEventKind::ReasoningEnd {
            summary: vec!["s".into()],
            content: vec!["c".into()]
        }
    );
}

#[test]
fn command_executions_start_stream_and_end() {
    let mut m = mapper();
    let start = only(item(
        &mut m,
        "item/started",
        json!({"type": "commandExecution", "id": "c1", "command": "ls -la", "cwd": "/tmp/x",
               "status": "inProgress", "commandActions": [{"type": "listFiles", "command": "ls"}]}),
    ));
    let AgentEventKind::ToolStart {
        tool,
        name,
        title,
        input,
    } = &start.kind
    else {
        panic!("{start:?}")
    };
    assert_eq!(
        (*tool, name.as_str(), title.as_str()),
        (ToolKind::Command, "shell", "ls -la")
    );
    assert_eq!(input.as_ref().unwrap()["cwd"], "/tmp/x");
    assert!(
        start.payload.is_some(),
        "the raw item is kept for tool events"
    );

    let out = only(notify(
        &mut m,
        "item/commandExecution/outputDelta",
        json!({"turnId": TURN, "itemId": "c1", "delta": "a\n"}),
    ));
    assert_eq!(
        out.kind,
        AgentEventKind::CommandOutput { text: "a\n".into() }
    );

    let end = only(item(
        &mut m,
        "item/completed",
        json!({"type": "commandExecution", "id": "c1", "command": "ls -la", "status": "completed",
               "aggregatedOutput": "a\n", "exitCode": 0, "durationMs": 12}),
    ));
    assert_eq!(
        end.kind,
        AgentEventKind::ToolEnd {
            tool: ToolKind::Command,
            status: ToolStatus::Completed,
            output: Some("a\n".into()),
            truncated: false,
            exit_code: Some(0),
            duration_ms: Some(12),
        }
    );
    assert_eq!(end.item_id, start.item_id);
}

#[test]
fn huge_command_output_is_cut_and_flagged() {
    let mut m = mapper();
    let output = "é".repeat(MAX_OUTPUT_BYTES); // 2 bytes per char: cuts mid-way, on a boundary
    let end = only(item(
        &mut m,
        "item/completed",
        json!({"type": "commandExecution", "id": "c1", "command": "yes", "status": "failed",
               "aggregatedOutput": output, "exitCode": 1}),
    ));
    let AgentEventKind::ToolEnd {
        output: Some(text),
        truncated,
        status,
        ..
    } = end.kind
    else {
        panic!()
    };
    assert!(truncated);
    assert_eq!(status, ToolStatus::Failed);
    assert!(text.len() <= MAX_OUTPUT_BYTES && text.len() > MAX_OUTPUT_BYTES - 4);
    assert!(end.payload.is_none(), "an oversized raw item is not kept");
}

#[test]
fn file_changes_become_tool_events_and_per_file_diffs() {
    let mut m = mapper();
    let started = item(
        &mut m,
        "item/started",
        json!({"type": "fileChange", "id": "f1", "status": "inProgress", "changes": [
            {"path": "/w/a.txt", "kind": {"type": "add"}, "diff": "hi\n"},
            {"path": "/w/b.txt", "kind": {"type": "update", "move_path": "/w/c.txt"}, "diff": "@@\n-x\n+y\n"}]}),
    );
    assert_eq!(started.len(), 2);
    let AgentEventKind::ToolStart { tool, title, .. } = &started[0].kind else {
        panic!()
    };
    assert_eq!(*tool, ToolKind::FileChange);
    assert_eq!(title, "edit /w/a.txt and 1 more");
    let AgentEventKind::FileDiff { changes, patch } = &started[1].kind else {
        panic!()
    };
    assert!(patch.is_none());
    assert_eq!(changes[1].move_path.as_deref(), Some("/w/c.txt"));
    assert_eq!(changes[0].patch.as_deref(), Some("hi\n"));

    let updated = only(notify(
        &mut m,
        "item/fileChange/patchUpdated",
        json!({"turnId": TURN, "itemId": "f1", "changes": [
            {"path": "/w/a.txt", "kind": {"type": "delete"}, "diff": ""}]}),
    ));
    let AgentEventKind::FileDiff { changes, .. } = updated.kind else {
        panic!()
    };
    assert_eq!(changes[0].patch, None, "an empty diff is no patch");

    let aggregate = only(notify(
        &mut m,
        "turn/diff/updated",
        json!({"turnId": TURN, "diff": "diff --git a/a b/a\n"}),
    ));
    assert_eq!(
        aggregate.kind,
        AgentEventKind::FileDiff {
            changes: vec![],
            patch: Some("diff --git a/a b/a\n".into())
        }
    );
}

#[test]
fn mcp_tool_calls_use_the_servers_content_as_output() {
    let mut m = mapper();
    let start = only(item(
        &mut m,
        "item/started",
        json!({"type": "mcpToolCall", "id": "t1", "server": "docs", "tool": "search",
               "status": "inProgress", "arguments": {"q": "x"}}),
    ));
    let AgentEventKind::ToolStart {
        tool,
        name,
        title,
        input,
    } = start.kind
    else {
        panic!()
    };
    assert_eq!(
        (tool, name.as_str(), title.as_str()),
        (ToolKind::Mcp, "docs.search", "docs/search")
    );
    assert_eq!(input, Some(json!({"q": "x"})));
    let progress = only(notify(
        &mut m,
        "item/mcpToolCall/progress",
        json!({"turnId": TURN, "itemId": "t1", "message": "50%"}),
    ));
    assert_eq!(
        progress.kind,
        AgentEventKind::ToolUpdate {
            message: "50%".into()
        }
    );
    let end = only(item(
        &mut m,
        "item/completed",
        json!({"type": "mcpToolCall", "id": "t1", "server": "docs", "tool": "search",
               "status": "completed", "result": {"content": [{"type": "text", "text": "found"}]}}),
    ));
    let AgentEventKind::ToolEnd { output, status, .. } = end.kind else {
        panic!()
    };
    assert_eq!(
        (output.as_deref(), status),
        (Some("found"), ToolStatus::Completed)
    );
    let failed = only(item(
        &mut m,
        "item/completed",
        json!({"type": "mcpToolCall", "id": "t2", "server": "docs", "tool": "search",
               "status": "failed", "error": {"message": "offline"}}),
    ));
    let AgentEventKind::ToolEnd { output, status, .. } = failed.kind else {
        panic!()
    };
    assert_eq!(
        (output.as_deref(), status),
        (Some("offline"), ToolStatus::Failed)
    );
}

#[test]
fn dynamic_tools_and_web_searches_are_tools_too() {
    let mut m = mapper();
    let dynamic = only(item(
        &mut m,
        "item/started",
        json!({"type": "dynamicToolCall", "id": "d1", "namespace": "app", "tool": "open",
               "status": "inProgress"}),
    ));
    assert!(matches!(
        &dynamic.kind,
        AgentEventKind::ToolStart { tool: ToolKind::Dynamic, name, .. } if name == "app.open"
    ));
    let end = only(item(
        &mut m,
        "item/completed",
        json!({"type": "dynamicToolCall", "id": "d1", "tool": "open", "status": "completed", "success": false}),
    ));
    assert!(matches!(
        end.kind,
        AgentEventKind::ToolEnd {
            status: ToolStatus::Failed,
            tool: ToolKind::Dynamic,
            ..
        }
    ));
    let search = only(item(
        &mut m,
        "item/started",
        json!({"type": "webSearch", "id": "w1", "query": "rust"}),
    ));
    assert!(matches!(
        &search.kind,
        AgentEventKind::ToolStart { tool: ToolKind::WebSearch, title, .. } if title == "rust"
    ));
}

#[test]
fn plans_come_as_steps_or_as_prose() {
    let mut m = mapper();
    let steps = only(notify(
        &mut m,
        "turn/plan/updated",
        json!({"turnId": TURN, "explanation": "why", "plan": [
            {"step": "a", "status": "completed"}, {"step": "b", "status": "inProgress"},
            {"step": "c", "status": "pending"}, {"step": "d", "status": "who knows"}]}),
    ));
    let AgentEventKind::Plan {
        explanation,
        steps,
        text,
    } = steps.kind
    else {
        panic!()
    };
    assert_eq!(explanation.as_deref(), Some("why"));
    assert!(text.is_none());
    let status: Vec<_> = steps.iter().map(|s| s.status).collect();
    assert_eq!(
        status,
        [
            PlanStepStatus::Completed,
            PlanStepStatus::InProgress,
            PlanStepStatus::Pending,
            PlanStepStatus::Pending
        ]
    );
    let prose = only(item(
        &mut m,
        "item/completed",
        json!({"type": "plan", "id": "p", "text": "do x"}),
    ));
    assert!(matches!(
        &prose.kind,
        AgentEventKind::Plan { text: Some(t), .. } if t == "do x"
    ));
    // A plan item is complete when it ends; nothing at its start.
    assert!(
        item(
            &mut m,
            "item/started",
            json!({"type": "plan", "id": "p", "text": ""})
        )
        .is_empty()
    );
}

#[test]
fn usage_keeps_missing_numbers_missing() {
    let mut m = mapper();
    let usage = only(notify(
        &mut m,
        "thread/tokenUsage/updated",
        json!({"turnId": TURN, "tokenUsage": {
            "total": {"totalTokens": 100, "inputTokens": 90, "outputTokens": 10},
            "modelContextWindow": 258400}}),
    ));
    let AgentEventKind::Usage {
        total,
        last,
        context_window,
    } = usage.kind
    else {
        panic!()
    };
    let total = total.unwrap();
    assert_eq!(
        (total.total_tokens, total.input_tokens, total.output_tokens),
        (Some(100), Some(90), Some(10))
    );
    assert_eq!(total.cached_input_tokens, None, "never zero-filled");
    assert_eq!(total.reasoning_output_tokens, None);
    assert!(last.is_none());
    assert_eq!(context_window, Some(258_400));
}

#[test]
fn errors_and_warnings_become_notices() {
    let mut m = mapper();
    let error = only(notify(
        &mut m,
        "error",
        json!({"turnId": TURN, "error": {"message": "bad"}, "willRetry": false}),
    ));
    assert_eq!(
        error.kind,
        AgentEventKind::Notice {
            level: NoticeLevel::Error,
            message: "bad".into()
        }
    );
    let retry = only(notify(
        &mut m,
        "error",
        json!({"turnId": TURN, "error": {"message": "slow"}, "willRetry": true}),
    ));
    assert!(matches!(
        &retry.kind,
        AgentEventKind::Notice { level: NoticeLevel::Warning, message } if message.contains("will retry") && message.contains("slow")
    ));
    let warning = only(notify(&mut m, "warning", json!({"message": "careful"})));
    assert_eq!(
        warning.kind,
        AgentEventKind::Notice {
            level: NoticeLevel::Warning,
            message: "careful".into()
        }
    );
}

#[test]
fn account_chatter_and_other_threads_are_dropped_unknowns_are_kept() {
    let mut m = mapper();
    for method in [
        "account/updated",
        "account/rateLimits/updated",
        "mcpServer/startupStatus/updated",
        "remoteControl/status/changed",
        "thread/status/changed",
        "thread/started",
        "deprecationNotice",
    ] {
        assert!(notify(&mut m, method, json!({})).is_empty(), "{method}");
    }
    let foreign = m.notification(
        "item/agentMessage/delta",
        json!({"threadId": "sub-agent", "turnId": "x", "itemId": "y", "delta": "z"}),
    );
    assert!(foreign.is_empty());
    let unknown = only(notify(
        &mut m,
        "brand/new",
        json!({"turnId": TURN, "a": [1, 2]}),
    ));
    assert_eq!(
        unknown.kind,
        AgentEventKind::Opaque {
            method: "brand/new".into()
        }
    );
    assert_eq!(unknown.payload.unwrap()["a"], json!([1, 2]));
    assert_eq!(unknown.provider.turn_id.as_deref(), Some(TURN));
    // Known method, wrong shape: kept as opaque too.
    let malformed = only(notify(
        &mut m,
        "item/agentMessage/delta",
        json!({"delta": 5}),
    ));
    assert_eq!(
        malformed.kind,
        AgentEventKind::Opaque {
            method: "item/agentMessage/delta".into()
        }
    );
}

fn command_request(m: &mut Mapper, id: i64, decisions: Option<Value>) -> AgentEventDraft {
    let mut params = json!({"threadId": THREAD, "turnId": TURN, "itemId": "c1",
        "command": "rm -rf build", "cwd": "/w", "reason": "cleanup"});
    if let Some(decisions) = decisions {
        params["availableDecisions"] = decisions;
    }
    let mapped = m.request(json!(id), "item/commandExecution/requestApproval", params);
    assert!(mapped.reply.is_none(), "approvals wait for a decision");
    only(mapped.drafts)
}

#[test]
fn offered_decisions_are_preserved_and_only_they_can_be_chosen() {
    let mut m = mapper();
    let amendment =
        json!({"acceptWithExecpolicyAmendment": {"execpolicy_amendment": ["rm", "-rf"]}});
    let draft = command_request(&mut m, 5, Some(json!(["accept", amendment, "cancel"])));
    let AgentEventKind::PermissionRequested {
        approval_id,
        options,
        detail,
        ..
    } = draft.kind
    else {
        panic!()
    };
    assert_eq!(detail.as_deref(), Some("cleanup\nin /w"));
    assert_eq!(
        options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["accept", "acceptWithExecpolicyAmendment", "cancel"]
    );
    assert_eq!(options[1].provider.as_ref().unwrap(), &amendment);
    assert_eq!(
        m.answer(approval_id, "decline"),
        Err(staffy_agents::codex::AnswerError::NotOffered(
            "decline".into()
        ))
    );
    let (rpc_id, result) = m
        .answer(approval_id, "acceptWithExecpolicyAmendment")
        .unwrap();
    assert_eq!(rpc_id, json!(5));
    assert_eq!(result, json!({"decision": amendment}));
    // Answered once.
    assert_eq!(
        m.answer(approval_id, "accept"),
        Err(staffy_agents::codex::AnswerError::Unknown)
    );
}

#[test]
fn a_resolved_request_reports_the_chosen_option_or_expiry() {
    let mut m = mapper();
    let answered = command_request(&mut m, 1, None);
    let AgentEventKind::PermissionRequested {
        approval_id: first,
        options,
        ..
    } = answered.kind
    else {
        panic!()
    };
    // The schema's decisions are the offer when the server lists none.
    assert_eq!(options.len(), 4);
    m.answer(first, "decline").unwrap();
    let resolved = only(notify(
        &mut m,
        "serverRequest/resolved",
        json!({"requestId": 1}),
    ));
    assert_eq!(
        resolved.kind,
        AgentEventKind::PermissionResolved {
            approval_id: first,
            outcome: PermissionOutcome::Selected {
                option_id: "decline".into()
            }
        }
    );

    // Withdrawn by the provider without our answer.
    let unanswered = command_request(&mut m, 2, None);
    let AgentEventKind::PermissionRequested {
        approval_id: second,
        ..
    } = unanswered.kind
    else {
        panic!()
    };
    let resolved = only(notify(
        &mut m,
        "serverRequest/resolved",
        json!({"requestId": 2}),
    ));
    assert_eq!(
        resolved.kind,
        AgentEventKind::PermissionResolved {
            approval_id: second,
            outcome: PermissionOutcome::Expired
        }
    );
    // Unknown request ids are ignored.
    assert!(notify(&mut m, "serverRequest/resolved", json!({"requestId": 99})).is_empty());
    assert!(!m.has_pending());
}

#[test]
fn a_turn_ending_resolves_what_is_still_pending_before_its_own_event() {
    let mut m = mapper();
    let draft = command_request(&mut m, 3, None);
    let AgentEventKind::PermissionRequested { approval_id, .. } = draft.kind else {
        panic!()
    };
    let drafts = notify(
        &mut m,
        "turn/completed",
        json!({"turn": {"id": TURN, "status": "interrupted"}}),
    );
    assert_eq!(drafts.len(), 2);
    assert_eq!(
        drafts[0].kind,
        AgentEventKind::PermissionResolved {
            approval_id,
            outcome: PermissionOutcome::Expired
        }
    );
    assert_eq!(drafts[1].kind, AgentEventKind::TurnCancelled);
    assert_eq!(
        m.answer(approval_id, "accept"),
        Err(staffy_agents::codex::AnswerError::Unknown)
    );
}

#[test]
fn permission_profiles_offer_grant_for_turn_session_or_deny() {
    let mut m = mapper();
    let permissions = json!({"network": {"enabled": true}});
    let mapped = m.request(
        json!("req-a"),
        "item/permissions/requestApproval",
        json!({"threadId": THREAD, "turnId": TURN, "itemId": "p1", "cwd": "/w",
               "permissions": permissions, "reason": "needs the network"}),
    );
    let draft = only(mapped.drafts);
    let AgentEventKind::PermissionRequested {
        approval_id,
        kind,
        options,
        ..
    } = draft.kind
    else {
        panic!()
    };
    assert_eq!(kind, PermissionKind::Permissions);
    assert_eq!(
        options.iter().map(|o| o.id.as_str()).collect::<Vec<_>>(),
        ["grant", "grant_session", "deny"]
    );
    let (rpc, result) = m.answer(approval_id, "grant_session").unwrap();
    assert_eq!(
        rpc,
        json!("req-a"),
        "string request ids are echoed as they came"
    );
    assert_eq!(
        result,
        json!({"permissions": permissions, "scope": "session"})
    );
}

#[test]
fn requests_staffy_cannot_serve_are_refused_not_ignored() {
    let mut m = mapper();
    let mapped = m.request(
        json!(8),
        "item/tool/requestUserInput",
        json!({"threadId": THREAD, "turnId": TURN, "itemId": "q", "questions": []}),
    );
    let reply = mapped.reply.expect("the agent must get an answer");
    assert_eq!(reply.id, json!(8));
    assert_eq!(reply.outcome.unwrap_err().0, -32601);
    assert!(matches!(
        only(mapped.drafts).kind,
        AgentEventKind::Notice {
            level: NoticeLevel::Warning,
            ..
        }
    ));
    // A malformed approval is refused the same way.
    let mapped = m.request(
        json!(9),
        "item/commandExecution/requestApproval",
        json!({"threadId": THREAD}),
    );
    assert!(mapped.reply.is_some());
    assert!(!m.has_pending());
}

#[test]
fn a_dead_process_expires_everything_pending() {
    let mut m = mapper();
    for id in [1, 2] {
        command_request(&mut m, id, None);
    }
    let drafts = m.expire_all();
    assert_eq!(drafts.len(), 2);
    assert!(drafts.iter().all(|d| matches!(
        d.kind,
        AgentEventKind::PermissionResolved {
            outcome: PermissionOutcome::Expired,
            ..
        }
    )));
    assert!(!m.has_pending());
}
