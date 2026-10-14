//! Translation of Codex and Claude Code hook events (JSON on stdin) into agent reports
//! (`heeler agent hook`). Pure apart from the subagent counter file, so it is tested directly.

use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::args::{AgentStateArg, HookAgentArg};

pub const CODEX_SOURCE: &str = "codex-hooks";
pub const CLAUDE_SOURCE: &str = "claude-hooks";

#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Report {
        agent: &'static str,
        source: &'static str,
        state: AgentStateArg,
        message: Option<String>,
        agent_session_id: Option<String>,
        resume_argv: Vec<String>,
    },
    Release {
        source: &'static str,
    },
}

/// What the translation needs to know about where it runs.
#[derive(Debug)]
pub struct HookEnv<'a> {
    /// `HEELER_SESSION_ID`; keys the subagent counter.
    pub heeler_session: &'a str,
    /// `CODEX_THREAD_ID`: the conversation of the Codex that spawned this hook's process tree.
    pub codex_thread_id: Option<&'a str>,
    /// Where the subagent counter lives.
    pub state_dir: &'a Path,
}

/// The directory for per-session hook state.
pub fn default_state_dir() -> PathBuf {
    std::env::temp_dir().join("heeler-agent")
}

/// The report (or release) one hook event stands for; `None` for events that say nothing about
/// the agent's state.
pub fn plan(agent: HookAgentArg, input: &Value, env: &HookEnv<'_>) -> Option<Plan> {
    let event = input.get("hook_event_name")?.as_str()?;
    let session_id = input
        .get("session_id")
        .and_then(Value::as_str)
        .filter(|id| is_plain_id(id));
    if agent == HookAgentArg::Codex
        && let (Some(thread), Some(id)) = (env.codex_thread_id, session_id)
        && thread != id
    {
        // A Codex started by another Codex's tool: its hooks are not about this session's agent.
        return None;
    }
    let (agent_name, source, resume_program, resume_flag): (
        &'static str,
        &'static str,
        &'static str,
        &'static [&'static str],
    ) = match agent {
        HookAgentArg::Codex => ("codex", CODEX_SOURCE, "codex", &["resume"]),
        HookAgentArg::Claude => ("claude", CLAUDE_SOURCE, "claude", &["--resume"]),
    };
    let counter = SubagentCounter::new(env.state_dir, env.heeler_session);

    let (state, message) = match event {
        "SessionStart" => {
            counter.reset();
            (AgentStateArg::Idle, None)
        }
        "Stop" | "StopFailure" => {
            counter.reset();
            (AgentStateArg::Idle, None)
        }
        "SessionEnd" => {
            counter.reset();
            return Some(Plan::Release { source });
        }
        // PostToolUse ends a permission prompt as far as hooks can tell.
        "UserPromptSubmit" | "PostToolUse" => (AgentStateArg::Working, counter.message()),
        "SubagentStart" => {
            counter.start();
            (AgentStateArg::Working, counter.message())
        }
        "SubagentStop" => {
            counter.stop();
            (AgentStateArg::Working, counter.message())
        }
        "PermissionRequest" => {
            let tool = input
                .get("tool_name")
                .and_then(Value::as_str)
                .filter(|tool| !tool.is_empty());
            let message = match tool {
                Some(tool) => format!("permission: {tool}"),
                None => "permission requested".to_owned(),
            };
            (AgentStateArg::Blocked, Some(message))
        }
        _ => return None,
    };
    let resume_argv = session_id
        .map(|id| {
            let mut argv = vec![resume_program.to_owned()];
            argv.extend(resume_flag.iter().map(|flag| (*flag).to_owned()));
            argv.push(id.to_owned());
            argv
        })
        .unwrap_or_default();
    Some(Plan::Report {
        agent: agent_name,
        source,
        state,
        message,
        agent_session_id: session_id.map(str::to_owned),
        resume_argv,
    })
}

/// Ids that are safe inside a resume command: the daemon rejects apostrophes and control
/// characters, and a hook must not lose its state report over an odd id.
fn is_plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 200
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
}

/// Running subagents of one session. Hooks of parallel subagents race, so the count is a file
/// of `+` and `-` marks appended with single writes and counted when read.
struct SubagentCounter {
    file: PathBuf,
}

impl SubagentCounter {
    fn new(dir: &Path, heeler_session: &str) -> Self {
        let name: String = heeler_session
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        Self {
            file: dir.join(format!("{name}.subagents")),
        }
    }

    fn mark(&self, mark: u8) {
        if let Some(dir) = self.file.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Ok(mut file) = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.file)
        {
            let _ = file.write_all(&[mark]);
        }
    }

    fn start(&self) {
        self.mark(b'+');
    }

    /// A stop nobody started (the counter was reset meanwhile) is not recorded, so the count
    /// never goes below zero for later starts.
    fn stop(&self) {
        if self.running() > 0 {
            self.mark(b'-');
        }
    }

    fn reset(&self) {
        let _ = fs::remove_file(&self.file);
    }

    fn running(&self) -> usize {
        let marks = fs::read(&self.file).unwrap_or_default();
        let started = marks.iter().filter(|m| **m == b'+').count();
        let stopped = marks.iter().filter(|m| **m == b'-').count();
        started.saturating_sub(stopped)
    }

    /// `subagents: N` while any run.
    fn message(&self) -> Option<String> {
        match self.running() {
            0 => None,
            n => Some(format!("subagents: {n}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn env(dir: &Path) -> HookEnv<'_> {
        HookEnv {
            heeler_session: "01K7F3B5N2Q9W8E4R6T1Y0V3KM",
            codex_thread_id: None,
            state_dir: dir,
        }
    }

    fn report(plan: Option<Plan>) -> (AgentStateArg, Option<String>, Vec<String>) {
        match plan {
            Some(Plan::Report {
                state,
                message,
                resume_argv,
                ..
            }) => (state, message, resume_argv),
            other => panic!("expected a report, got {other:?}"),
        }
    }

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|w| (*w).to_owned()).collect()
    }

    #[test]
    fn claude_events_map_to_states_and_a_resume_command() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let event = |name: &str| json!({"hook_event_name": name, "session_id": "5f3e-1c"});
        let go = |name: &str| plan(HookAgentArg::Claude, &event(name), &env);

        let (state, message, resume) = report(go("SessionStart"));
        assert_eq!((state, message), (AgentStateArg::Idle, None));
        assert_eq!(resume, argv(&["claude", "--resume", "5f3e-1c"]));
        assert_eq!(report(go("UserPromptSubmit")).0, AgentStateArg::Working);
        assert_eq!(report(go("PostToolUse")).0, AgentStateArg::Working);
        assert_eq!(report(go("Stop")).0, AgentStateArg::Idle);
        assert_eq!(report(go("StopFailure")).0, AgentStateArg::Idle);
        assert_eq!(
            go("SessionEnd"),
            Some(Plan::Release {
                source: CLAUDE_SOURCE
            })
        );
        assert_eq!(go("PreCompact"), None);
        assert_eq!(
            plan(HookAgentArg::Claude, &json!({"session_id": "x"}), &env),
            None
        );
    }

    #[test]
    fn permission_requests_are_blocked_with_the_tool_name() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let input = json!({
            "hook_event_name": "PermissionRequest",
            "session_id": "abc",
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf build"},
        });
        let (state, message, _) = report(plan(HookAgentArg::Claude, &input, &env));
        assert_eq!(state, AgentStateArg::Blocked);
        assert_eq!(message.as_deref(), Some("permission: Bash"));
    }

    #[test]
    fn subagents_are_counted_in_the_message_until_the_turn_ends() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let go = |name: &str| {
            report(plan(
                HookAgentArg::Claude,
                &json!({"hook_event_name": name, "session_id": "abc"}),
                &env,
            ))
        };
        assert_eq!(go("SubagentStart").1.as_deref(), Some("subagents: 1"));
        assert_eq!(go("SubagentStart").1.as_deref(), Some("subagents: 2"));
        assert_eq!(go("PostToolUse").1.as_deref(), Some("subagents: 2"));
        assert_eq!(go("SubagentStop").1.as_deref(), Some("subagents: 1"));
        assert_eq!(go("SubagentStop").1, None);
        assert_eq!(go("SubagentStop").1, None, "never below zero");
        assert_eq!(go("SubagentStart").1.as_deref(), Some("subagents: 1"));
        assert_eq!(go("Stop").1, None);
        assert_eq!(
            go("UserPromptSubmit").1,
            None,
            "a finished turn forgets them"
        );
    }

    #[test]
    fn codex_resumes_with_its_own_command_and_ignores_foreign_threads() {
        let dir = tempfile::tempdir().unwrap();
        let mut env = env(dir.path());
        let input = json!({"hook_event_name": "SessionStart", "session_id": "0198-abc"});
        let (_, _, resume) = report(plan(HookAgentArg::Codex, &input, &env));
        assert_eq!(resume, argv(&["codex", "resume", "0198-abc"]));

        env.codex_thread_id = Some("0198-abc");
        assert!(plan(HookAgentArg::Codex, &input, &env).is_some());
        env.codex_thread_id = Some("some-other-thread");
        assert_eq!(plan(HookAgentArg::Codex, &input, &env), None);
    }

    #[test]
    fn odd_session_ids_never_cost_the_state_report() {
        let dir = tempfile::tempdir().unwrap();
        let env = env(dir.path());
        let input = json!({"hook_event_name": "Stop", "session_id": "it's"});
        let (state, _, resume) = report(plan(HookAgentArg::Claude, &input, &env));
        assert_eq!(state, AgentStateArg::Idle);
        assert!(resume.is_empty());
    }
}
