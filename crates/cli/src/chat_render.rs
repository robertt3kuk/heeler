//! Plain-text rendering of chat events for `hutan chat send --wait`, `log` and `attach`.
//!
//! Assistant text streams as it arrives; a tool call is one line when it ends
//! (`• shell: touch hi.txt → completed (exit 0, 12 ms)`); a permission request is a short block
//! with the command to answer it. Reasoning, usage and streamed command output are left out
//! (`--json` has every event).

use std::collections::{HashMap, HashSet};

use hutan_core::agent::{MessageRole, NoticeLevel, PermissionOutcome, ToolStatus};
use hutan_core::question::FieldKind;
use hutan_core::{AgentEvent, AgentEventKind, ItemId, QuestionOutcome};

/// Longest tool title shown on its line, in characters.
const MAX_TITLE_CHARS: usize = 100;

/// Turns events into the text to print, remembering what is needed across events.
#[derive(Debug, Default)]
pub struct ChatPrinter {
    /// Leave out the user's own messages (`send --wait`: you just typed it).
    skip_user_messages: bool,
    /// `(tool name, title)` of tool calls that started and did not end yet.
    tools: HashMap<ItemId, (String, String)>,
    /// Assistant messages whose text was already printed as deltas.
    streamed: HashSet<ItemId>,
    /// The output ends in the middle of a line.
    open_line: bool,
}

impl ChatPrinter {
    pub fn new() -> Self {
        Self::default()
    }

    /// A printer that does not repeat the user's own messages.
    pub fn without_user_messages() -> Self {
        Self {
            skip_user_messages: true,
            ..Self::default()
        }
    }

    /// The text to print for `event` (possibly nothing). Text that does not end in a newline
    /// is a partial line: print it as is and flush.
    pub fn render(&mut self, event: &AgentEvent) -> String {
        match &event.kind {
            AgentEventKind::MessageDelta { text } => {
                if let Some(item) = event.item_id {
                    self.streamed.insert(item);
                }
                self.open_line = !text.ends_with('\n');
                text.clone()
            }
            // The journal's snapshot of a message that is still streaming (`chat log`, or the
            // backlog of `chat attach`): what was written so far, as if it had just streamed.
            // The deltas that follow (`attach`) and the end event carry on from it.
            AgentEventKind::MessagePartial {
                message_id, text, ..
            } => {
                self.streamed.insert(*message_id);
                self.open_line = !text.ends_with('\n');
                text.clone()
            }
            AgentEventKind::MessageEnd { role, text, .. } => match role {
                MessageRole::Assistant => {
                    let streamed = event
                        .item_id
                        .is_some_and(|item| self.streamed.remove(&item));
                    if streamed {
                        self.close_line()
                    } else {
                        let mut out = self.close_line();
                        out.push_str(text);
                        if !text.ends_with('\n') {
                            out.push('\n');
                        }
                        out
                    }
                }
                MessageRole::User if self.skip_user_messages => String::new(),
                MessageRole::User => {
                    let mut out = self.close_line();
                    out.push_str("> ");
                    out.push_str(&text.trim_end().replace('\n', "\n  "));
                    out.push('\n');
                    out
                }
            },
            AgentEventKind::ToolStart { name, title, .. } => {
                if let Some(item) = event.item_id {
                    self.tools.insert(item, (name.clone(), title.clone()));
                }
                String::new()
            }
            AgentEventKind::ToolEnd {
                status,
                exit_code,
                duration_ms,
                ..
            } => {
                let (name, title) = event
                    .item_id
                    .and_then(|item| self.tools.remove(&item))
                    .unwrap_or_else(|| ("tool".to_owned(), String::new()));
                let mut out = self.close_line();
                out.push_str(&format!(
                    "• {name}: {} → {}",
                    one_line(&title),
                    status_text(*status)
                ));
                let mut details = Vec::new();
                if let Some(code) = exit_code {
                    details.push(format!("exit {code}"));
                }
                if let Some(ms) = duration_ms {
                    details.push(format!("{ms} ms"));
                }
                if !details.is_empty() {
                    out.push_str(&format!(" ({})", details.join(", ")));
                }
                out.push('\n');
                out
            }
            AgentEventKind::PermissionRequested {
                approval_id,
                title,
                detail,
                options,
                ..
            } => {
                let mut out = self.close_line();
                out.push_str(&format!("? approval {approval_id}: {}\n", one_line(title)));
                if let Some(detail) = detail {
                    for line in detail.lines().take(3) {
                        out.push_str(&format!("    {line}\n"));
                    }
                }
                let ids: Vec<&str> = options.iter().map(|o| o.id.as_str()).collect();
                out.push_str(&format!(
                    "    answer with: hutan approve {approval_id} <{}>\n",
                    ids.join("|")
                ));
                out
            }
            AgentEventKind::PermissionResolved {
                approval_id,
                outcome,
                decided_by,
            } => {
                let mut out = self.close_line();
                match outcome {
                    PermissionOutcome::Selected { option_id } => {
                        let by = match decided_by {
                            Some(by) => match &by.client_name {
                                Some(name) => {
                                    format!(" (decided by {}: {name})", by.client_kind)
                                }
                                None => format!(" (decided by {})", by.client_kind),
                            },
                            None => String::new(),
                        };
                        out.push_str(&format!("  approval {approval_id}: {option_id}{by}\n"));
                    }
                    PermissionOutcome::Expired => {
                        out.push_str(&format!("  approval {approval_id}: expired\n"));
                    }
                }
                out
            }
            AgentEventKind::QuestionRequested {
                question_id, spec, ..
            } => {
                let mut out = self.close_line();
                out.push_str(&format!(
                    "? question {question_id}: {}\n",
                    one_line(&spec.title)
                ));
                for line in spec.prompt.lines().filter(|l| !l.trim().is_empty()).take(3) {
                    out.push_str(&format!("    {line}\n"));
                }
                for field in &spec.fields {
                    let kind = match &field.kind {
                        FieldKind::Text => "text".to_owned(),
                        FieldKind::Multiline => "multiline".to_owned(),
                        FieldKind::Secret => "secret".to_owned(),
                        FieldKind::Boolean => "true|false".to_owned(),
                        FieldKind::Number { .. } => "number".to_owned(),
                        FieldKind::Choice { options, .. } => options
                            .iter()
                            .map(|o| o.value.as_str())
                            .collect::<Vec<_>>()
                            .join("|"),
                    };
                    let required = if field.required { "" } else { "?" };
                    out.push_str(&format!(
                        "    {}{required} = <{}>  {}\n",
                        field.id,
                        one_line(&kind),
                        one_line(&field.label)
                    ));
                }
                out.push_str(&format!(
                    "    answer with: hutan answer {question_id} <field=value ...|--decline|--cancel>\n"
                ));
                out
            }
            AgentEventKind::QuestionResolved {
                question_id,
                outcome,
                decided_by,
            } => {
                let mut out = self.close_line();
                let by = match decided_by {
                    Some(by) => match &by.client_name {
                        Some(name) => format!(" (decided by {}: {name})", by.client_kind),
                        None => format!(" (decided by {})", by.client_kind),
                    },
                    None => String::new(),
                };
                let what = match outcome {
                    QuestionOutcome::Answered { withheld, .. } if withheld.is_empty() => {
                        "answered".to_owned()
                    }
                    // A secret's value is not in the journal: only that it was answered.
                    QuestionOutcome::Answered { withheld, .. } => {
                        format!("answered (secret: {})", withheld.join(", "))
                    }
                    QuestionOutcome::Declined => "declined".to_owned(),
                    QuestionOutcome::Cancelled => "cancelled".to_owned(),
                    QuestionOutcome::Expired => "expired".to_owned(),
                };
                out.push_str(&format!("  question {question_id}: {what}{by}\n"));
                out
            }
            AgentEventKind::Plan { steps, text, .. } => {
                let mut out = self.close_line();
                if steps.is_empty() {
                    let first = text.as_deref().unwrap_or("").lines().next().unwrap_or("");
                    out.push_str(&format!("• plan: {}\n", one_line(first)));
                } else {
                    let done = steps
                        .iter()
                        .filter(|s| s.status == hutan_core::agent::PlanStepStatus::Completed)
                        .count();
                    out.push_str(&format!("• plan: {done}/{} steps done\n", steps.len()));
                }
                out
            }
            AgentEventKind::TurnCompleted { .. } => self.close_line(),
            AgentEventKind::TurnFailed { message, .. } => {
                let mut out = self.close_line();
                out.push_str(&format!("✗ turn failed: {}\n", one_line(message)));
                out
            }
            AgentEventKind::TurnCancelled => {
                let mut out = self.close_line();
                out.push_str("— turn cancelled\n");
                out
            }
            AgentEventKind::Notice { level, message } => {
                let mut out = self.close_line();
                let label = match level {
                    NoticeLevel::Info => "",
                    NoticeLevel::Warning => "warning: ",
                    NoticeLevel::Error => "error: ",
                };
                out.push_str(&format!("! {label}{}\n", one_line(message)));
                out
            }
            AgentEventKind::TurnStarted
            | AgentEventKind::MessageStart { .. }
            | AgentEventKind::ReasoningStart
            | AgentEventKind::ReasoningPartial { .. }
            | AgentEventKind::ReasoningDelta { .. }
            | AgentEventKind::ReasoningEnd { .. }
            | AgentEventKind::ToolUpdate { .. }
            | AgentEventKind::CommandOutput { .. }
            | AgentEventKind::FileDiff { .. }
            | AgentEventKind::Usage { .. }
            | AgentEventKind::ResumeLocator { .. }
            | AgentEventKind::Opaque { .. } => String::new(),
        }
    }

    /// What ends the output after the last event: a newline when it stopped mid-line (a
    /// message that was still streaming).
    pub fn finish(&mut self) -> String {
        self.close_line()
    }

    /// A newline when the output stopped mid-line, else nothing.
    fn close_line(&mut self) -> String {
        if std::mem::take(&mut self.open_line) {
            "\n".to_owned()
        } else {
            String::new()
        }
    }
}

/// The first line of `text`, cut to a readable length.
fn one_line(text: &str) -> String {
    let first = text.lines().next().unwrap_or("");
    let multi = text.lines().nth(1).is_some();
    let mut shown: String = first.chars().take(MAX_TITLE_CHARS).collect();
    if multi || first.chars().count() > MAX_TITLE_CHARS {
        shown.push('…');
    }
    shown
}

fn status_text(status: ToolStatus) -> &'static str {
    match status {
        ToolStatus::Completed => "completed",
        ToolStatus::Failed => "failed",
        ToolStatus::Declined => "declined",
    }
}

#[cfg(test)]
mod tests {
    use hutan_core::DecidedBy;
    use hutan_core::agent::{PermissionKind, PermissionOption, PermissionOptionKind, ToolKind};
    use hutan_core::{AgentEventDraft, ApprovalId, SessionId};

    use super::*;

    fn event(item: Option<ItemId>, kind: AgentEventKind) -> AgentEvent {
        let mut draft = AgentEventDraft::new(kind);
        draft.item_id = item;
        draft.into_event(SessionId::new(), 1, 1)
    }

    #[test]
    fn streamed_text_is_not_printed_twice_and_replayed_text_is_printed_once() {
        let mut p = ChatPrinter::new();
        let item = ItemId::new();
        let live: String = ["Hel", "lo"]
            .iter()
            .map(|t| {
                p.render(&event(
                    Some(item),
                    AgentEventKind::MessageDelta {
                        text: (*t).to_owned(),
                    },
                ))
            })
            .collect();
        let end = |text: &str| {
            event(
                Some(item),
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text: text.to_owned(),
                },
            )
        };
        assert_eq!(live + &p.render(&end("Hello")), "Hello\n");

        // From the journal there are no deltas: the end carries the text.
        let mut p = ChatPrinter::new();
        assert_eq!(p.render(&end("Hello")), "Hello\n");
        assert_eq!(p.render(&end("Hello\n")), "Hello\n");
    }

    #[test]
    fn a_tool_call_is_one_line_with_its_outcome() {
        let mut p = ChatPrinter::new();
        let item = ItemId::new();
        assert_eq!(
            p.render(&event(
                Some(item),
                AgentEventKind::ToolStart {
                    tool: ToolKind::Command,
                    name: "shell".into(),
                    title: "touch hi.txt".into(),
                    input: None,
                }
            )),
            ""
        );
        let end = p.render(&event(
            Some(item),
            AgentEventKind::ToolEnd {
                tool: ToolKind::Command,
                status: ToolStatus::Completed,
                output: Some("ignored\n".into()),
                truncated: false,
                exit_code: Some(0),
                duration_ms: Some(12),
            },
        ));
        assert_eq!(end, "• shell: touch hi.txt → completed (exit 0, 12 ms)\n");
    }

    #[test]
    fn long_and_multiline_titles_are_cut() {
        assert_eq!(one_line("a\nb"), "a…");
        assert_eq!(one_line(&"x".repeat(150)).chars().count(), 101);
        assert_eq!(one_line("short"), "short");
    }

    #[test]
    fn a_request_tells_how_to_answer_it() {
        let mut p = ChatPrinter::new();
        let approval = ApprovalId::new();
        let out = p.render(&event(
            None,
            AgentEventKind::PermissionRequested {
                approval_id: approval,
                kind: PermissionKind::Command,
                title: "rm -rf build".into(),
                detail: Some("cleanup\nin /w".into()),
                options: vec![
                    PermissionOption {
                        id: "accept".into(),
                        label: "Allow once".into(),
                        kind: PermissionOptionKind::AllowOnce,
                        provider: None,
                    },
                    PermissionOption {
                        id: "cancel".into(),
                        label: "Deny".into(),
                        kind: PermissionOptionKind::Cancel,
                        provider: None,
                    },
                ],
            },
        ));
        assert!(
            out.starts_with(&format!("? approval {approval}: rm -rf build\n")),
            "{out}"
        );
        assert!(out.contains("    cleanup\n    in /w\n"), "{out}");
        assert!(
            out.contains(&format!("hutan approve {approval} <accept|cancel>")),
            "{out}"
        );
    }

    #[test]
    fn a_resolution_says_who_answered() {
        let mut p = ChatPrinter::new();
        let approval = ApprovalId::new();
        let resolved = |outcome, decided_by| {
            event(
                None,
                AgentEventKind::PermissionResolved {
                    approval_id: approval,
                    outcome,
                    decided_by,
                },
            )
        };
        let selected = || PermissionOutcome::Selected {
            option_id: "accept".into(),
        };
        let by = |kind: &str, name: Option<&str>| {
            Some(DecidedBy {
                client_kind: kind.into(),
                client_name: name.map(str::to_owned),
            })
        };
        assert_eq!(
            p.render(&resolved(selected(), by("cli", None))),
            format!("  approval {approval}: accept (decided by cli)\n")
        );
        assert_eq!(
            p.render(&resolved(selected(), by("desktop", Some("window-2")))),
            format!("  approval {approval}: accept (decided by desktop: window-2)\n")
        );
        // Journaled before protocol 0.15, or nobody answered.
        assert_eq!(
            p.render(&resolved(selected(), None)),
            format!("  approval {approval}: accept\n")
        );
        assert_eq!(
            p.render(&resolved(PermissionOutcome::Expired, None)),
            format!("  approval {approval}: expired\n")
        );
    }

    #[test]
    fn a_snapshot_of_a_streaming_message_prints_its_text_and_the_stream_carries_on() {
        let mut p = ChatPrinter::new();
        let item = ItemId::new();
        let out = p.render(&event(
            Some(item),
            AgentEventKind::MessagePartial {
                message_id: item,
                phase: None,
                text: "so far".into(),
            },
        ));
        assert_eq!(out, "so far");
        // `attach`: the live deltas continue on the same line, the end adds nothing.
        assert_eq!(
            p.render(&event(
                Some(item),
                AgentEventKind::MessageDelta {
                    text: " and on".into()
                },
            )),
            " and on"
        );
        assert_eq!(
            p.render(&event(
                Some(item),
                AgentEventKind::MessageEnd {
                    role: MessageRole::Assistant,
                    phase: None,
                    text: "so far and on".into(),
                },
            )),
            "\n"
        );
        // `log`: a snapshot is the last thing; the line is closed at the end.
        let mut p = ChatPrinter::new();
        p.render(&event(
            Some(item),
            AgentEventKind::MessagePartial {
                message_id: item,
                phase: None,
                text: "cut".into(),
            },
        ));
        assert_eq!(p.finish(), "\n");
        assert_eq!(p.finish(), "");
    }

    #[test]
    fn a_line_in_progress_is_closed_before_other_output() {
        let mut p = ChatPrinter::new();
        let item = ItemId::new();
        let delta = p.render(&event(
            Some(item),
            AgentEventKind::MessageDelta {
                text: "partial".into(),
            },
        ));
        assert_eq!(delta, "partial");
        let failed = p.render(&event(
            None,
            AgentEventKind::TurnFailed {
                message: "boom".into(),
                details: None,
            },
        ));
        assert_eq!(failed, "\n✗ turn failed: boom\n");
    }

    #[test]
    fn quiet_events_print_nothing() {
        let mut p = ChatPrinter::new();
        for kind in [
            AgentEventKind::TurnStarted,
            AgentEventKind::ReasoningStart,
            AgentEventKind::CommandOutput { text: "x".into() },
            AgentEventKind::Opaque { method: "m".into() },
            AgentEventKind::TurnCompleted { duration_ms: None },
        ] {
            assert_eq!(p.render(&event(None, kind)), "");
        }
    }
}
