//! `heeler agents`: the agents and subagents the daemon observes (`agent.tree`), as a tree per
//! session; with `--watch`, again whenever a tree changes.

use heeler_client::Client;
use heeler_core::{
    AgentNode, AgentObservation, Liveness, NodeOutcome, NodeState, SessionId, Usage,
};
use heeler_protocol::messages::{Empty, Event, Request, SessionInfo, Subscribed};
use tokio::sync::broadcast::error::RecvError;

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::AgentsArgs;
use crate::error::CliError;

impl Ctx {
    pub(super) async fn agents(&self, args: AgentsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        if !client.supports_agent_tree() {
            return Err(CliError::Failed(
                "this daemon does not observe agent trees (protocol 0.10); restart it with a newer heelerd"
                    .to_owned(),
            ));
        }
        let sessions = self.session_list(&client, None).await?.sessions;
        let session = match &args.target {
            Some(target) => Some(super::sessions::resolve_target(&sessions, target)?.id),
            None => None,
        };
        // Subscribe first so a change between the print and the wait is not lost.
        let mut events = args.watch.then(|| client.events());
        if args.watch {
            let _: Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        self.print_trees(&client, session).await?;
        let Some(events) = events.as_mut() else {
            return Ok(());
        };
        loop {
            match events.recv().await {
                Ok((_, Event::AgentTreeUpdated { session_id, .. }))
                    if session.is_none_or(|id| id == session_id) =>
                {
                    self.print_trees(&client, session).await?;
                }
                Ok(_) | Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return Ok(()),
            }
        }
    }

    async fn print_trees(
        &self,
        client: &Client,
        session: Option<SessionId>,
    ) -> Result<(), CliError> {
        let observations = client.agent_tree(session).await.map_err(|e| self.fail(e))?;
        let sessions = self.session_list(client, None).await?.sessions;
        self.print(&observations, || {
            tree_report(&observations, &sessions, now_ms())
        })
    }
}

/// Every tree, headed by its session.
fn tree_report(observations: &[AgentObservation], sessions: &[SessionInfo], now_ms: u64) -> String {
    if observations.is_empty() {
        return "no agents".to_owned();
    }
    observations
        .iter()
        .map(|observation| {
            let title = sessions
                .iter()
                .find(|s| s.id == observation.session_id)
                .map_or("", |s| s.title.as_str());
            format!(
                "{} {title}\n{}",
                observation.session_id,
                render_tree(observation, now_ms)
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// One tree: indentation by depth, a state glyph, the label, the model, how long it has been
/// there, and tokens when the source reports them.
pub(super) fn render_tree(observation: &AgentObservation, now_ms: u64) -> String {
    let mut out = String::new();
    fn walk(
        observation: &AgentObservation,
        node: &AgentNode,
        depth: usize,
        now_ms: u64,
        out: &mut String,
    ) {
        out.push_str(&"  ".repeat(depth + 1));
        out.push_str(&node_line(node, now_ms));
        out.push('\n');
        for child in observation
            .nodes
            .values()
            .filter(|n| n.parent.as_deref() == Some(node.node_id.as_str()))
        {
            walk(observation, child, depth + 1, now_ms, out);
        }
    }
    if let Some(root) = observation.nodes.get(&observation.root) {
        walk(observation, root, 0, now_ms, &mut out);
    }
    out.trim_end().to_owned()
}

fn glyph(node: &AgentNode) -> &'static str {
    match (node.outcome, node.state) {
        (Some(NodeOutcome::Completed), _) => "✓",
        (Some(NodeOutcome::Failed), _) => "✗",
        (Some(NodeOutcome::Interrupted), _) => "■",
        (Some(NodeOutcome::Disconnected), _) => "⊘",
        (None, NodeState::Working) => "●",
        (None, NodeState::Blocked) => "▲",
        (None, NodeState::Idle) => "○",
        (None, NodeState::Unknown) => "?",
    }
}

fn node_line(node: &AgentNode, now_ms: u64) -> String {
    let mut parts = vec![format!("{} {}", glyph(node), node.label())];
    let status = match (node.outcome, node.state) {
        (Some(outcome), _) => format!("{outcome:?}").to_lowercase(),
        (None, state) => format!("{state:?}").to_lowercase(),
    };
    parts.push(status);
    if node.liveness == Liveness::Unknown && node.outcome.is_none() {
        parts.push("(liveness unknown)".to_owned());
    }
    if let Some(model) = &node.model {
        parts.push(model.clone());
    }
    if let Some(started) = node.started_at_ms {
        let end = node.ended_at_ms.unwrap_or(now_ms);
        parts.push(age_text(end.saturating_sub(started) / 1000));
    }
    if let Some(tokens) = node.usage.as_ref().and_then(tokens_text) {
        parts.push(tokens);
    }
    if let Some(activity) = &node.activity {
        parts.push(format!("— {activity}"));
    }
    parts.join("  ")
}

/// Tokens as the source counted them, labelled by what the counter means; nothing when the
/// source reported no token figure (never a made-up zero).
fn tokens_text(usage: &Usage) -> Option<String> {
    let total = match (usage.input, usage.output) {
        (None, None) => usage.context_tokens?,
        (input, output) => input.unwrap_or(0) + output.unwrap_or(0),
    };
    Some(format!("{} tok [{}]", compact(total), usage.kind))
}

fn compact(n: u64) -> String {
    match n {
        0..=999 => n.to_string(),
        1_000..=999_999 => format!("{:.1}k", n as f64 / 1e3),
        _ => format!("{:.1}M", n as f64 / 1e6),
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use heeler_core::{NodeKind, NodeOutcome};

    use super::*;

    fn node(id: &str, parent: Option<&str>, state: NodeState) -> AgentNode {
        AgentNode {
            node_id: id.into(),
            parent: parent.map(str::to_owned),
            kind: if parent.is_some() {
                NodeKind::Sub
            } else {
                NodeKind::Main
            },
            source: "s".into(),
            agent: "claude".into(),
            name: None,
            agent_type: None,
            state,
            outcome: None,
            liveness: Liveness::Alive,
            model: None,
            started_at_ms: Some(1_000),
            ended_at_ms: None,
            last_activity_ms: 1_000,
            activity: None,
            usage: None,
            transcript_path: None,
        }
    }

    #[test]
    fn the_tree_is_indented_with_state_glyphs_models_durations_and_tokens() {
        let mut root = node("root", None, NodeState::Working);
        root.model = Some("claude-opus-5".into());
        let mut explore = node("a1", Some("root"), NodeState::Blocked);
        explore.name = Some("Explore".into());
        explore.activity = Some("permission: Bash".into());
        let mut done = node("a2", Some("a1"), NodeState::Idle);
        done.name = Some("Grep".into());
        done.outcome = Some(NodeOutcome::Completed);
        done.ended_at_ms = Some(61_000);
        done.usage = Some(Usage {
            kind: "omp.tokens".into(),
            input: Some(1_200),
            output: Some(300),
            ..Usage::default()
        });
        let observation = AgentObservation {
            session_id: SessionId::new(),
            root: "root".into(),
            nodes: [root, explore, done]
                .into_iter()
                .map(|n| (n.node_id.clone(), n))
                .collect::<BTreeMap<_, _>>(),
        };
        let text = render_tree(&observation, 31_000);
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(
            lines[0].starts_with("  ● root  working  claude-opus-5  30s"),
            "{text}"
        );
        assert!(lines[1].starts_with("    ▲ Explore  blocked"), "{text}");
        assert!(lines[1].ends_with("— permission: Bash"), "{text}");
        assert!(lines[2].starts_with("      ✓ Grep  completed"), "{text}");
        assert!(lines[2].contains("1.5k tok [omp.tokens]"), "{text}");
    }

    #[test]
    fn no_token_figure_prints_nothing() {
        let mut n = node("root", None, NodeState::Idle);
        n.usage = Some(Usage {
            kind: "claude.context".into(),
            context_window: Some(200_000),
            ..Usage::default()
        });
        assert!(!node_line(&n, 2_000).contains("tok"));
    }
}
