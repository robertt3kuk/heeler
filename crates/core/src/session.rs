//! Session kinds and the single lifecycle shared by every kind (architecture.md § Domain model).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    /// PTY-backed terminal running a shell or any TUI.
    Terminal,
    /// Structured coding-agent session (Codex app-server, ACP).
    Harness,
    /// Conversation through Hutan's own model gateway.
    Direct,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SessionState {
    Starting,
    /// Process alive; for agent sessions a turn is in progress.
    Running,
    /// Agent session alive and waiting for user input.
    Idle,
    WaitingApproval,
    Exited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Failed {
        reason: String,
    },
    /// Ended because a client asked to stop it.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionEvent {
    Spawned,
    /// An agent session finished starting (process initialized, conversation open) and waits
    /// for the first message; unlike `Spawned` no turn is in progress.
    Ready,
    TurnStarted,
    TurnFinished,
    ApprovalRequested,
    ApprovalResolved,
    ProcessExited {
        code: Option<i32>,
        signal: Option<i32>,
    },
    Failed {
        reason: String,
    },
    StopCompleted,
    Restart,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid session transition from {from:?} on {event:?}")]
pub struct InvalidTransition {
    pub from: SessionState,
    pub event: SessionEvent,
}

impl SessionState {
    /// The session still owns a live process (or is about to).
    pub const fn is_live(&self) -> bool {
        matches!(
            self,
            Self::Starting | Self::Running | Self::Idle | Self::WaitingApproval
        )
    }

    pub fn transition(&self, event: SessionEvent) -> Result<Self, InvalidTransition> {
        use SessionEvent as E;
        use SessionState as S;

        let next = match (self, &event) {
            (S::Starting, E::Spawned) => S::Running,
            (S::Starting, E::Ready) => S::Idle,
            (S::Running, E::TurnFinished) => S::Idle,
            (S::Idle, E::TurnStarted) => S::Running,
            (S::Running, E::ApprovalRequested) => S::WaitingApproval,
            (S::WaitingApproval, E::ApprovalResolved) => S::Running,
            (live, E::ProcessExited { code, signal }) if live.is_live() => S::Exited {
                code: *code,
                signal: *signal,
            },
            (live, E::Failed { reason }) if live.is_live() => S::Failed {
                reason: reason.clone(),
            },
            (live, E::StopCompleted) if live.is_live() => S::Stopped,
            (ended, E::Restart) if !ended.is_live() => S::Starting,
            _ => {
                return Err(InvalidTransition {
                    from: self.clone(),
                    event,
                });
            }
        };
        Ok(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ended_states() -> [SessionState; 3] {
        [
            SessionState::Exited {
                code: Some(0),
                signal: None,
            },
            SessionState::Failed {
                reason: "spawn".into(),
            },
            SessionState::Stopped,
        ]
    }

    fn live_states() -> [SessionState; 4] {
        [
            SessionState::Starting,
            SessionState::Running,
            SessionState::Idle,
            SessionState::WaitingApproval,
        ]
    }

    #[test]
    fn ended_sessions_only_accept_restart() {
        for state in ended_states() {
            assert_eq!(
                state.transition(SessionEvent::Restart),
                Ok(SessionState::Starting)
            );
            for event in [
                SessionEvent::Spawned,
                SessionEvent::TurnStarted,
                SessionEvent::ApprovalResolved,
                SessionEvent::StopCompleted,
                SessionEvent::ProcessExited {
                    code: Some(1),
                    signal: None,
                },
            ] {
                assert!(state.transition(event).is_err(), "{state:?} accepted event");
            }
        }
    }

    #[test]
    fn any_live_state_can_end_and_keeps_exit_status() {
        for state in live_states() {
            assert_eq!(
                state.transition(SessionEvent::ProcessExited {
                    code: None,
                    signal: Some(9)
                }),
                Ok(SessionState::Exited {
                    code: None,
                    signal: Some(9)
                })
            );
            assert_eq!(
                state.transition(SessionEvent::StopCompleted),
                Ok(SessionState::Stopped)
            );
            assert!(state.transition(SessionEvent::Restart).is_err());
        }
    }

    #[test]
    fn agent_turn_and_approval_cycle() {
        let s = SessionState::Starting;
        let s = s.transition(SessionEvent::Spawned).unwrap();
        let s = s.transition(SessionEvent::ApprovalRequested).unwrap();
        assert_eq!(s, SessionState::WaitingApproval);
        assert!(s.transition(SessionEvent::TurnFinished).is_err());
        let s = s.transition(SessionEvent::ApprovalResolved).unwrap();
        let s = s.transition(SessionEvent::TurnFinished).unwrap();
        assert_eq!(s, SessionState::Idle);
        assert!(s.transition(SessionEvent::ApprovalRequested).is_err());
        assert_eq!(
            s.transition(SessionEvent::TurnStarted),
            Ok(SessionState::Running)
        );
    }

    #[test]
    fn state_wire_format_is_tagged() {
        let json = serde_json::to_string(&SessionState::Exited {
            code: Some(2),
            signal: None,
        })
        .unwrap();
        assert_eq!(json, r#"{"state":"exited","code":2,"signal":null}"#);
    }
}
