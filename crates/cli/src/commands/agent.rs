//! `heeler agent report | release | hook`: the reporting side of the agent channel. These are
//! called by agent hooks and extensions, so they never fail, never print, and never wait: no
//! `HEELER_SESSION_ID`, no daemon, a daemon error, or 200 ms of silence all end in exit 0.

use std::io::{IsTerminal as _, Read as _};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use heeler_core::SessionId;
use heeler_protocol::messages::{
    AgentApplied, AgentReleaseRequest, AgentReportRequest, AgentState, Request,
};
use serde_json::Value;
use tokio::time::timeout;

use super::Ctx;
use crate::args::{AgentCommand, AgentStateArg, HookAgentArg};
use crate::hook::{self, HookEnv, Plan};

/// Everything — connecting included — must be over within this time.
const BUDGET: Duration = Duration::from_millis(200);
/// Hook payloads (prompts, tool inputs) are cut here; the fields that matter come first.
const MAX_HOOK_INPUT: u64 = 1024 * 1024;

pub(super) async fn run(ctx: Option<Ctx>, command: &AgentCommand) {
    // Taken first: the order in which hooks start is the order of their events.
    let default_seq = nanos_since_epoch();
    // Always consumed, so the agent writing it never meets a closed pipe.
    let hook_input = match command {
        AgentCommand::Hook(_) => read_hook_input(),
        AgentCommand::Report(_) | AgentCommand::Release(_) => None,
    };
    let Some(ctx) = ctx else { return };
    let Some(session) = session_from_env() else {
        return;
    };
    let request = match command {
        AgentCommand::Report(args) => Request::AgentReport(AgentReportRequest {
            session_id: session,
            source: args.source.clone(),
            agent: args.agent.clone(),
            state: state(args.state),
            message: args.message.clone(),
            seq: args.seq.unwrap_or(default_seq),
            agent_session_id: args.agent_session_id.clone(),
            resume_argv: (!args.resume_argv.is_empty()).then(|| args.resume_argv.clone()),
        }),
        AgentCommand::Release(args) => Request::AgentRelease(AgentReleaseRequest {
            session_id: session,
            source: args.source.clone(),
            seq: args.seq.unwrap_or(default_seq),
        }),
        AgentCommand::Hook(args) => {
            let Some(input) = hook_input else { return };
            let Some(request) = hook_request(args.agent, &input, session, default_seq) else {
                return;
            };
            request
        }
    };
    // Nothing to do with the outcome: a hook cannot act on it, and must not be noisy about it.
    let _ = timeout(BUDGET, async {
        let client = ctx.connect().await.ok()?;
        client.call::<AgentApplied>(request).await.ok()
    })
    .await;
}

fn hook_request(
    agent: HookAgentArg,
    input: &Value,
    session: SessionId,
    seq: u64,
) -> Option<Request> {
    let state_dir = hook::default_state_dir();
    let session_text = session.to_string();
    let codex_thread = std::env::var("CODEX_THREAD_ID").ok();
    let env = HookEnv {
        heeler_session: &session_text,
        codex_thread_id: codex_thread.as_deref(),
        state_dir: &state_dir,
    };
    Some(match hook::plan(agent, input, &env)? {
        Plan::Report {
            agent,
            source,
            state: report_state,
            message,
            agent_session_id,
            resume_argv,
        } => Request::AgentReport(AgentReportRequest {
            session_id: session,
            source: source.to_owned(),
            agent: agent.to_owned(),
            state: state(report_state),
            message,
            seq,
            agent_session_id,
            resume_argv: (!resume_argv.is_empty()).then_some(resume_argv),
        }),
        Plan::Release { source } => Request::AgentRelease(AgentReleaseRequest {
            session_id: session,
            source: source.to_owned(),
            seq,
        }),
    })
}

fn state(arg: AgentStateArg) -> AgentState {
    match arg {
        AgentStateArg::Idle => AgentState::Idle,
        AgentStateArg::Working => AgentState::Working,
        AgentStateArg::Blocked => AgentState::Blocked,
    }
}

/// The Heeler session this process runs in, from the environment the daemon gave it.
fn session_from_env() -> Option<SessionId> {
    std::env::var("HEELER_SESSION_ID").ok()?.parse().ok()
}

/// The JSON document the agent wrote to the hook's stdin; `None` when there is none.
fn read_hook_input() -> Option<Value> {
    let mut stdin = std::io::stdin();
    if stdin.is_terminal() {
        return None;
    }
    let mut text = String::new();
    stdin
        .by_ref()
        .take(MAX_HOOK_INPUT)
        .read_to_string(&mut text)
        .ok()?;
    serde_json::from_str(&text).ok()
}

fn nanos_since_epoch() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}
