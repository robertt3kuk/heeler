//! The client-side model of daemon state: what [`Connection::state`](crate::Connection::state)
//! publishes and how events fold into it. Pure data and logic, no I/O.

use std::time::Duration;

use heeler_core::{Epoch, SessionId, TerminalId};
use heeler_protocol::blocks::Block;
use heeler_protocol::messages::{Event, SessionInfo, SessionSnapshot, WorkspaceInfo};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    /// The first connection attempt is in progress.
    Connecting,
    /// Handshaken and synchronised: the rest of [`StoreState`] is current.
    Connected,
    /// The connection was lost (or could not be made); the next attempt starts in `retry_in`.
    /// The rest of [`StoreState`] is the last known view.
    Reconnecting { attempt: u32, retry_in: Duration },
    /// [`Connection::shutdown`](crate::Connection::shutdown) ran; nothing will change anymore.
    Stopped,
}

/// Latest known daemon state.
#[derive(Debug, Clone, PartialEq)]
pub struct StoreState {
    pub connection: ConnectionState,
    /// Epoch of the daemon this state describes; `None` before the first synchronisation.
    pub epoch: Option<Epoch>,
    /// The daemon's manual order (`position`); Home usually comes first. Sort modes, pins and
    /// filters are the UI's business, from the fields of [`WorkspaceInfo`].
    pub workspaces: Vec<WorkspaceInfo>,
    /// Creation order (`created_at_ms`, then id).
    pub sessions: Vec<SessionInfo>,
}

/// Transient events that are not part of [`StoreState`].
#[derive(Debug, Clone, PartialEq)]
pub enum Notice {
    /// The daemon's epoch changed: every terminal of the old daemon is gone.
    DaemonRestarted {
        old: Epoch,
        new: Epoch,
    },
    SessionExited {
        session: SessionId,
        code: Option<i32>,
        signal: Option<i32>,
    },
    Bell {
        terminal: TerminalId,
    },
    TerminalNotification {
        terminal: TerminalId,
        title: Option<String>,
        body: String,
    },
    /// A block of the terminal started, changed state or was truncated (`terminal.block`,
    /// protocol 0.5). Fold it into a [`BlockList`](crate::BlockList).
    TerminalBlock {
        terminal: TerminalId,
        block: Block,
    },
}

/// Side effects of folding events into the state, for the caller to act on after publishing.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Effects {
    pub notices: Vec<Notice>,
    /// Every session update folded, in order: the session and its terminal afterwards. A
    /// restart shows up as the same session with another terminal, possibly after an update
    /// without any (the daemon publishes the restart before the new terminal exists), which is
    /// why consumers track which session owns a terminal instead of diffing the state.
    pub session_terminals: Vec<(SessionId, Option<TerminalId>)>,
    /// Terminals that disappeared together with their session.
    pub removed: Vec<TerminalId>,
}

impl Effects {
    pub(crate) fn extend(&mut self, other: Effects) {
        self.notices.extend(other.notices);
        self.session_terminals.extend(other.session_terminals);
        self.removed.extend(other.removed);
    }
}

fn session_key(session: &SessionInfo) -> (u64, SessionId) {
    (session.created_at_ms, session.id)
}

impl StoreState {
    pub(crate) fn initial() -> Self {
        Self {
            connection: ConnectionState::Connecting,
            epoch: None,
            workspaces: Vec::new(),
            sessions: Vec::new(),
        }
    }

    /// Replaces workspaces and sessions with `snapshot` (the daemon's view at one seq).
    pub(crate) fn install_snapshot(&mut self, snapshot: SessionSnapshot) -> Effects {
        let effects = Effects {
            session_terminals: snapshot
                .sessions
                .iter()
                .map(|s| (s.id, s.terminal_id))
                .collect(),
            ..Effects::default()
        };
        self.workspaces = snapshot.workspaces;
        self.workspaces.sort_by_key(|w| w.position);
        self.sessions = snapshot.sessions;
        self.sessions.sort_by_key(session_key);
        effects
    }

    /// Drops everything learned from a previous daemon.
    pub(crate) fn clear(&mut self) {
        self.workspaces.clear();
        self.sessions.clear();
    }

    /// Folds one event into the state. Events that only concern terminals' presentation (title)
    /// and unknown events change nothing.
    pub(crate) fn apply(&mut self, event: Event) -> Effects {
        let mut effects = Effects::default();
        match event {
            Event::WorkspaceUpdated { workspace } => {
                match self.workspaces.iter_mut().find(|w| w.id == workspace.id) {
                    Some(existing) => *existing = workspace,
                    None => self.workspaces.push(workspace),
                }
                // A move announces every changed position; the list follows them (stable, so
                // equal positions keep their arrival order).
                self.workspaces.sort_by_key(|w| w.position);
            }
            Event::WorkspaceClosed { workspace_id } => {
                self.workspaces.retain(|w| w.id != workspace_id);
                // The daemon removes a closed workspace's sessions without separate events.
                self.sessions.retain(|session| {
                    let keep = session.workspace_id != workspace_id;
                    if !keep {
                        effects.removed.extend(session.terminal_id);
                    }
                    keep
                });
            }
            Event::SessionUpdated { session } => {
                effects
                    .session_terminals
                    .push((session.id, session.terminal_id));
                match self.sessions.iter().position(|s| s.id == session.id) {
                    Some(at) => self.sessions[at] = session,
                    None => {
                        let at = self
                            .sessions
                            .partition_point(|s| session_key(s) < session_key(&session));
                        self.sessions.insert(at, session);
                    }
                }
            }
            Event::SessionExited {
                session_id,
                code,
                signal,
            } => effects.notices.push(Notice::SessionExited {
                session: session_id,
                code,
                signal,
            }),
            Event::TerminalBell { terminal_id } => effects.notices.push(Notice::Bell {
                terminal: terminal_id,
            }),
            Event::TerminalNotification {
                terminal_id,
                title,
                body,
            } => effects.notices.push(Notice::TerminalNotification {
                terminal: terminal_id,
                title,
                body,
            }),
            Event::TerminalBlock { terminal_id, block } => {
                effects.notices.push(Notice::TerminalBlock {
                    terminal: terminal_id,
                    block,
                });
            }
            Event::TerminalTitle { .. } => {}
        }
        effects
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use heeler_core::{SessionKind, SessionState, WorkspaceId};
    use heeler_protocol::messages::{CreatedBy, WorkspaceKind};

    use super::*;

    fn session(
        workspace_id: WorkspaceId,
        created_at_ms: u64,
        terminal: Option<u64>,
    ) -> SessionInfo {
        SessionInfo {
            id: SessionId::new(),
            workspace_id,
            kind: SessionKind::Terminal,
            title: "t".into(),
            state: SessionState::Running,
            command: Vec::new(),
            cwd: None,
            terminal_id: terminal.map(TerminalId),
            created_at_ms,
            agent: None,
            restored: false,
        }
    }

    fn workspace(name: &str) -> WorkspaceInfo {
        WorkspaceInfo {
            id: WorkspaceId::new(),
            name: name.into(),
            roots: vec![PathBuf::from("/r")],
            kind: WorkspaceKind::Project,
            pinned: false,
            position: 0,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
        }
    }

    #[test]
    fn workspaces_keep_arrival_order_and_update_in_place() {
        let mut state = StoreState::initial();
        let (a, b) = (workspace("a"), workspace("b"));
        state.apply(Event::WorkspaceUpdated {
            workspace: a.clone(),
        });
        state.apply(Event::WorkspaceUpdated {
            workspace: b.clone(),
        });
        let renamed = WorkspaceInfo {
            name: "a2".into(),
            ..a
        };
        state.apply(Event::WorkspaceUpdated {
            workspace: renamed.clone(),
        });
        assert_eq!(state.workspaces, vec![renamed, b]);
    }

    #[test]
    fn workspace_updates_reorder_the_list_by_position() {
        let mut state = StoreState::initial();
        let at = |name: &str, position| WorkspaceInfo {
            position,
            ..workspace(name)
        };
        let (home, a, b) = (at("home", 0), at("a", 1), at("b", 2));
        for w in [&b, &home, &a] {
            state.apply(Event::WorkspaceUpdated {
                workspace: w.clone(),
            });
        }
        assert_eq!(state.workspaces, vec![home.clone(), a.clone(), b.clone()]);

        // `b` moves before `a`: the daemon announces both new positions.
        let b_moved = WorkspaceInfo {
            position: 1,
            ..b.clone()
        };
        let a_moved = WorkspaceInfo {
            position: 2,
            ..a.clone()
        };
        for w in [&b_moved, &a_moved] {
            state.apply(Event::WorkspaceUpdated {
                workspace: (*w).clone(),
            });
        }
        assert_eq!(state.workspaces, vec![home.clone(), b_moved, a_moved]);

        // A snapshot installs the daemon's order too.
        let snapshot = SessionSnapshot {
            workspaces: vec![b.clone(), a.clone(), home.clone()],
            sessions: Vec::new(),
            seq: heeler_core::Seq(1),
        };
        state.install_snapshot(snapshot);
        assert_eq!(state.workspaces, vec![home, a, b]);
    }

    #[test]
    fn sessions_sort_by_creation_and_upsert_without_duplicates() {
        let mut state = StoreState::initial();
        let ws = WorkspaceId::new();
        let late = session(ws, 20, Some(2));
        let early = session(ws, 10, Some(1));
        for s in [late.clone(), early.clone(), early.clone()] {
            state.apply(Event::SessionUpdated { session: s });
        }
        assert_eq!(state.sessions, vec![early, late]);
    }

    #[test]
    fn session_updates_report_the_terminal_even_through_a_restart_gap() {
        let mut state = StoreState::initial();
        let first = session(WorkspaceId::new(), 1, Some(5));
        let gap = SessionInfo {
            terminal_id: None,
            ..first.clone()
        };
        let restarted = SessionInfo {
            terminal_id: Some(TerminalId(6)),
            ..first.clone()
        };
        let mut effects = Effects::default();
        for s in [first.clone(), gap, restarted] {
            effects.extend(state.apply(Event::SessionUpdated { session: s }));
        }
        assert_eq!(
            effects.session_terminals,
            vec![
                (first.id, Some(TerminalId(5))),
                (first.id, None),
                (first.id, Some(TerminalId(6)))
            ]
        );
        assert_eq!(state.sessions.len(), 1);
        assert_eq!(state.sessions[0].terminal_id, Some(TerminalId(6)));
    }

    #[test]
    fn closing_a_workspace_removes_its_sessions() {
        let mut state = StoreState::initial();
        let ws = workspace("w");
        let keep = session(WorkspaceId::new(), 1, Some(1));
        let gone = session(ws.id, 2, Some(2));
        state.apply(Event::WorkspaceUpdated {
            workspace: ws.clone(),
        });
        for s in [keep.clone(), gone] {
            state.apply(Event::SessionUpdated { session: s });
        }
        let effects = state.apply(Event::WorkspaceClosed {
            workspace_id: ws.id,
        });
        assert!(state.workspaces.is_empty());
        assert_eq!(state.sessions, vec![keep]);
        assert_eq!(effects.removed, vec![TerminalId(2)]);
    }

    #[test]
    fn snapshot_install_reports_every_session_terminal_and_sorts() {
        let mut state = StoreState::initial();
        let ws = WorkspaceId::new();
        let old = session(ws, 1, Some(1));
        state.apply(Event::SessionUpdated {
            session: old.clone(),
        });
        let restarted = SessionInfo {
            terminal_id: Some(TerminalId(9)),
            ..old
        };
        let later = session(ws, 5, Some(3));
        let effects = state.install_snapshot(SessionSnapshot {
            workspaces: Vec::new(),
            sessions: vec![later.clone(), restarted.clone()],
            seq: heeler_core::Seq(4),
        });
        assert_eq!(
            effects.session_terminals,
            vec![
                (later.id, Some(TerminalId(3))),
                (restarted.id, Some(TerminalId(9)))
            ]
        );
        assert_eq!(state.sessions, vec![restarted, later]);
    }

    #[test]
    fn block_events_become_notices() {
        use heeler_protocol::blocks::BlockState;
        let mut state = StoreState::initial();
        let block = Block {
            id: 3,
            prompt_line: 10,
            command_line: Some(10),
            output_start: Some(11),
            end_line: None,
            command: Some("make".into()),
            exit_code: None,
            started_at_ms: 5,
            finished_at_ms: None,
            cwd: None,
            state: BlockState::Running,
        };
        let effects = state.apply(Event::TerminalBlock {
            terminal_id: TerminalId(4),
            block: block.clone(),
        });
        assert_eq!(
            effects.notices,
            vec![Notice::TerminalBlock {
                terminal: TerminalId(4),
                block
            }]
        );
    }

    #[test]
    fn exit_bell_and_notification_become_notices() {
        let mut state = StoreState::initial();
        let id = SessionId::new();
        let mut effects = Effects::default();
        effects.extend(state.apply(Event::SessionExited {
            session_id: id,
            code: Some(3),
            signal: None,
        }));
        effects.extend(state.apply(Event::TerminalBell {
            terminal_id: TerminalId(1),
        }));
        effects.extend(state.apply(Event::TerminalTitle {
            terminal_id: TerminalId(1),
            title: None,
        }));
        assert_eq!(
            effects.notices,
            vec![
                Notice::SessionExited {
                    session: id,
                    code: Some(3),
                    signal: None
                },
                Notice::Bell {
                    terminal: TerminalId(1)
                },
            ]
        );
    }
}
