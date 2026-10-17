//! Restart recovery (protocol 0.26, capability `session.recovery`): what a daemon start did
//! to each session it found in the database, and the request that dismisses it
//! (`docs/protocol.md` § Recovery).
//!
//! Every field is a fact the daemon had at start: nothing says a program "kept running" through
//! a restart, because no program does (ADR 0009: processes die with the daemon).

use std::path::PathBuf;

use hutan_core::{Epoch, SessionId, TaskId};
use serde::{Deserialize, Serialize};

/// What a daemon start did to a session. `SessionInfo.recovery` is set when the start restored
/// the session, and cleared by `session.recovery_ack` or by the session's next user action
/// (terminal input, `chat.send`, `session.stop`, `session.restart`). It is not stored: a later
/// start computes it again from what it finds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recovery {
    pub kind: RecoveryKind,
    /// The run of the daemon that ended; absent when the database has no record of one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_epoch: Option<Epoch>,
    /// How that run ended, as far as the database knows.
    pub daemon_stop: DaemonStop,
    /// When the daemon that restored the session started (Unix milliseconds).
    pub restarted_at_ms: u64,
    /// What the restart cost the session.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub lost: Vec<Lost>,
}

/// What became of the session at the start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RecoveryKind {
    /// A terminal was alive when the previous daemon ended: a new shell runs in the same
    /// folder. Its previous command line was not run again, its screen and scrollback are gone.
    ShellRespawned {
        /// The folder the new shell started in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathBuf>,
        /// The command that was running in the foreground when the previous daemon ended (the
        /// last running terminal block, secrets removed), when the daemon had recorded one.
        /// It is not running now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        was_running: Option<String>,
    },
    /// A terminal whose coding agent had reported a resume command: the terminal runs that
    /// command now instead of a shell.
    AgentResumed { resume_argv: Vec<String> },
    /// An agent chat started again: the agent continues its stored conversation (or a new
    /// thread of it, see `thread_id`) and the journal keeps the history.
    ChatResumed {
        /// The agent's conversation id it was asked to continue; absent when the session had
        /// none, so the agent started a new conversation next to the kept journal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thread_id: Option<String>,
        last_turn: LastTurn,
    },
    /// The session is listed, not running: `restore_sessions = "stopped"`, its program is
    /// missing, its stored spec unusable, or the resume failed. `reason` says which. Restart
    /// with `session.restart`.
    NotRestarted { reason: String },
    /// It had ended before the restart (during the run that ended); listed for history only.
    EndedBefore,
}

/// How the last turn of a chat ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LastTurn {
    /// Finished, failed, or cancelled by the user before the restart.
    Completed,
    /// The daemon went away in the middle of it: the agent never finished its answer.
    Interrupted,
}

/// How the daemon run before this one ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DaemonStop {
    /// It shut itself down (`hutan daemon stop`, a signal, a deleted socket): it hung up on
    /// its sessions on purpose.
    Clean { at_ms: u64 },
    /// It never recorded a stop: it crashed, was killed, or the machine lost power.
    Crashed,
    /// The database does not say: no earlier run is recorded, or a build that did not track it
    /// wrote the record.
    Unknown,
}

/// Something the restart cost a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Lost {
    /// Permission requests nobody answered; their agent is gone, so they expired.
    Approvals { count: u32 },
    /// Questions of the agent nobody answered (`chat.questions`); their agent is gone, so they
    /// expired.
    Questions { count: u32 },
    /// The turn the agent was in the middle of.
    Turn,
    /// A running task whose session did not survive; it was moved to `failed`.
    Task { task_id: TaskId, title: String },
}

/// `session.recovery_ack`: dismisses the recovery of one session, or of every session when
/// `session_id` is absent. Answered with [`RecoveryAcked`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAck {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<SessionId>,
}

/// The sessions whose recovery was cleared (those that had none are not listed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryAcked {
    pub sessions: Vec<SessionId>,
}
