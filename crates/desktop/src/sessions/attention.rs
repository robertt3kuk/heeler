//! Attention (design.md § Attention model) for M2: terminal sessions only.
//!
//! `failed` (exit ≠ 0, a signal, a spawn failure) and `done` (exit 0, or a terminal
//! notification) are *unread* until the session's view is focused; everything else is idle.
//! Agent states (blocked, approval, working) arrive with M4/M7.
//!
//! GPUI-free and unit tested: [`AttentionTracker`] watches successive session lists and
//! remembers what the user has not seen yet.

use std::collections::{BTreeMap, HashMap};

use heeler_core::{Epoch, SessionId, SessionState};
use heeler_protocol::messages::SessionInfo;

/// Why a session wants a look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unread {
    /// Exited 0 or sent a terminal notification.
    Done,
    /// Exited non-zero, was killed by a signal, or failed to start.
    Failed,
}

/// Color role of a state chip (mapped to theme colors by the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Warning,
    Success,
    Accent,
    Muted,
    Error,
}

/// How a session shows up in lists, tabs and the status bar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Presentation {
    /// Short state word: `idle`, `done`, `failed`, `stopped`, …
    pub label: String,
    /// Subtitle detail such as `exit 2` or `signal 9`; empty when there is nothing to add.
    pub detail: String,
    pub tone: Tone,
    /// Filled dot (needs a look or is active) rather than hollow.
    pub filled: bool,
    pub needs_you: bool,
}

/// What the unread flag of a state transition should become when a live session ends.
fn unread_for_end(state: &SessionState) -> Option<Unread> {
    match state {
        SessionState::Exited {
            code: Some(0),
            signal: None,
        } => Some(Unread::Done),
        SessionState::Exited { .. } | SessionState::Failed { .. } => Some(Unread::Failed),
        // The user asked for it; there is nothing to look at.
        SessionState::Stopped => None,
        _ => None,
    }
}

/// Presentation of `info` given its unread flag.
pub fn present(info: &SessionInfo, unread: Option<Unread>) -> Presentation {
    let exit_detail = |code: Option<i32>, signal: Option<i32>| match (code, signal) {
        (_, Some(sig)) => format!("signal {sig}"),
        (Some(code), None) => format!("exit {code}"),
        (None, None) => String::new(),
    };
    match &info.state {
        SessionState::Starting => Presentation {
            label: "starting".into(),
            detail: String::new(),
            tone: Tone::Accent,
            filled: false,
            needs_you: false,
        },
        // A terminal notification from a running session: it has something to show.
        SessionState::Running | SessionState::Idle if unread.is_some() => Presentation {
            label: "done".into(),
            detail: "notification".into(),
            tone: Tone::Success,
            filled: true,
            needs_you: true,
        },
        SessionState::Running | SessionState::Idle => Presentation {
            label: "idle".into(),
            detail: String::new(),
            tone: Tone::Muted,
            filled: false,
            needs_you: false,
        },
        SessionState::WaitingApproval => Presentation {
            label: "approval".into(),
            detail: String::new(),
            tone: Tone::Warning,
            filled: true,
            needs_you: true,
        },
        SessionState::Exited { code, signal } => {
            let detail = exit_detail(*code, *signal);
            match unread {
                Some(Unread::Done) => Presentation {
                    label: "done".into(),
                    detail,
                    tone: Tone::Success,
                    filled: true,
                    needs_you: true,
                },
                Some(Unread::Failed) => Presentation {
                    label: "failed".into(),
                    detail,
                    tone: Tone::Error,
                    filled: true,
                    needs_you: true,
                },
                None => Presentation {
                    label: "exited".into(),
                    detail,
                    tone: if matches!((code, signal), (Some(0), None)) {
                        Tone::Muted
                    } else {
                        Tone::Error
                    },
                    filled: false,
                    needs_you: false,
                },
            }
        }
        SessionState::Failed { reason } => Presentation {
            label: "failed".into(),
            detail: reason.clone(),
            tone: Tone::Error,
            filled: unread.is_some(),
            needs_you: unread.is_some(),
        },
        SessionState::Stopped => Presentation {
            label: "stopped".into(),
            detail: String::new(),
            tone: Tone::Muted,
            filled: false,
            needs_you: false,
        },
    }
}

/// The one-line status a terminal view shows when its session ended.
pub fn ended_text(state: &SessionState) -> Option<String> {
    match state {
        SessionState::Exited {
            code: Some(0),
            signal: None,
        } => Some("The process exited with code 0".to_owned()),
        SessionState::Exited {
            code,
            signal: Some(sig),
        } => Some(format!(
            "The process was killed by signal {sig}{}",
            code.map(|c| format!(" (code {c})")).unwrap_or_default()
        )),
        SessionState::Exited {
            code: Some(code), ..
        } => Some(format!("The process exited with code {code}")),
        SessionState::Exited { .. } => Some("The process exited".to_owned()),
        SessionState::Failed { reason } => Some(format!("The session failed: {reason}")),
        SessionState::Stopped => Some("The session was stopped".to_owned()),
        _ => None,
    }
}

/// Remembers which sessions the user has not looked at since they ended.
#[derive(Debug, Default)]
pub struct AttentionTracker {
    unread: BTreeMap<SessionId, Unread>,
    /// Whether each known session was live in the last list; `None` until the first synchronised
    /// list of the current daemon epoch has been taken as the baseline.
    baseline: Option<HashMap<SessionId, bool>>,
    epoch: Option<Epoch>,
}

impl AttentionTracker {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn unread(&self, id: SessionId) -> Option<Unread> {
        self.unread.get(&id).copied()
    }

    /// Sessions flagged unread.
    pub fn unread_ids(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.unread.keys().copied()
    }

    /// Looks at the latest synchronised session list. The first list of an epoch is the
    /// baseline (what was already over when the desktop arrived is not news); later lists flag
    /// sessions that were live and now ended, and clear the flag of sessions that restarted.
    pub fn observe(&mut self, epoch: Option<Epoch>, sessions: &[SessionInfo]) {
        if self.epoch != epoch {
            self.epoch = epoch;
            self.baseline = None;
            self.unread.clear();
        }
        let now: HashMap<SessionId, bool> =
            sessions.iter().map(|s| (s.id, s.state.is_live())).collect();
        if let Some(before) = &self.baseline {
            for session in sessions {
                let was_live = before.get(&session.id).copied();
                if session.state.is_live() {
                    // A restart (ended before, live now) starts a new life; a live session
                    // keeps a notification until the user looks.
                    if was_live == Some(false) {
                        self.unread.remove(&session.id);
                    }
                } else if was_live == Some(true)
                    && let Some(kind) = unread_for_end(&session.state)
                {
                    self.unread.insert(session.id, kind);
                }
            }
        }
        self.unread.retain(|id, _| now.contains_key(id));
        self.baseline = Some(now);
    }

    /// A terminal notification arrived for `session`: it wants a look unless something worse is
    /// already flagged.
    pub fn notified(&mut self, session: SessionId) {
        if self
            .baseline
            .as_ref()
            .is_some_and(|b| b.contains_key(&session))
        {
            self.unread.entry(session).or_insert(Unread::Done);
        }
    }

    /// The user looked at `session`.
    pub fn mark_read(&mut self, session: SessionId) -> bool {
        self.unread.remove(&session).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use heeler_core::WorkspaceId;

    fn info(id: SessionId, state: SessionState) -> SessionInfo {
        SessionInfo {
            id,
            workspace_id: WorkspaceId::new(),
            kind: heeler_core::SessionKind::Terminal,
            title: "t".into(),
            state,
            command: vec![],
            cwd: None,
            terminal_id: None,
            created_at_ms: 0,
        }
    }

    fn exited(code: i32) -> SessionState {
        SessionState::Exited {
            code: Some(code),
            signal: None,
        }
    }

    #[test]
    fn the_first_list_is_a_baseline_not_news() {
        let (a, b) = (SessionId::new(), SessionId::new());
        let mut tracker = AttentionTracker::new();
        tracker.observe(
            Some(Epoch::new()),
            &[info(a, exited(1)), info(b, SessionState::Running)],
        );
        assert_eq!(tracker.unread(a), None);
        assert_eq!(tracker.unread(b), None);
    }

    #[test]
    fn live_to_ended_flags_done_or_failed_and_stop_flags_nothing() {
        let (ok, bad, sig, stopped, failed) = (
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
            SessionId::new(),
        );
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        let all = |state: &dyn Fn(SessionId) -> SessionState| {
            [ok, bad, sig, stopped, failed]
                .into_iter()
                .map(|id| info(id, state(id)))
                .collect::<Vec<_>>()
        };
        tracker.observe(epoch, &all(&|_| SessionState::Running));
        tracker.observe(
            epoch,
            &all(&|id| {
                if id == ok {
                    exited(0)
                } else if id == bad {
                    exited(2)
                } else if id == sig {
                    SessionState::Exited {
                        code: None,
                        signal: Some(9),
                    }
                } else if id == stopped {
                    SessionState::Stopped
                } else {
                    SessionState::Failed {
                        reason: "spawn".into(),
                    }
                }
            }),
        );
        assert_eq!(tracker.unread(ok), Some(Unread::Done));
        assert_eq!(tracker.unread(bad), Some(Unread::Failed));
        assert_eq!(tracker.unread(sig), Some(Unread::Failed));
        assert_eq!(tracker.unread(failed), Some(Unread::Failed));
        assert_eq!(tracker.unread(stopped), None);
    }

    #[test]
    fn read_restart_and_removal_clear_the_flag() {
        let (a, b, c) = (SessionId::new(), SessionId::new(), SessionId::new());
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        let live = |ids: &[SessionId]| {
            ids.iter()
                .map(|id| info(*id, SessionState::Running))
                .collect::<Vec<_>>()
        };
        tracker.observe(epoch, &live(&[a, b, c]));
        tracker.observe(
            epoch,
            &[info(a, exited(0)), info(b, exited(1)), info(c, exited(1))],
        );
        assert!(tracker.mark_read(a));
        assert!(!tracker.mark_read(a));
        tracker.observe(
            epoch,
            &[info(a, exited(0)), info(b, SessionState::Starting)],
        );
        assert_eq!(tracker.unread(a), None, "reading is sticky");
        assert_eq!(tracker.unread(b), None, "a restart clears it");
        assert_eq!(tracker.unread(c), None, "a removed session is forgotten");
    }

    #[test]
    fn notifications_flag_known_sessions_once() {
        let (a, ghost) = (SessionId::new(), SessionId::new());
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        tracker.notified(a);
        assert_eq!(
            tracker.unread(a),
            None,
            "nothing is known before the baseline"
        );
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        tracker.notified(ghost);
        assert_eq!(tracker.unread(ghost), None);
        tracker.notified(a);
        assert_eq!(tracker.unread(a), Some(Unread::Done));
    }

    #[test]
    fn a_notification_survives_later_updates_of_a_live_session() {
        let a = SessionId::new();
        let epoch = Some(Epoch::new());
        let mut tracker = AttentionTracker::new();
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        tracker.notified(a);
        assert_eq!(tracker.unread(a), Some(Unread::Done));
        tracker.observe(epoch, &[info(a, SessionState::Running)]);
        assert_eq!(tracker.unread(a), Some(Unread::Done), "still unread");
        let live = present(&info(a, SessionState::Running), tracker.unread(a));
        assert_eq!((live.label.as_str(), live.needs_you), ("done", true));
    }

    #[test]
    fn a_new_epoch_starts_over() {
        let (a, b, c) = (SessionId::new(), SessionId::new(), SessionId::new());
        let mut tracker = AttentionTracker::new();
        let epoch = Some(Epoch::new());
        let running = [a, b, c].map(|id| info(id, SessionState::Running));
        tracker.observe(epoch, &running);
        tracker.observe(
            epoch,
            &[
                info(a, exited(1)),
                info(b, SessionState::Running),
                info(c, exited(0)),
            ],
        );
        tracker.observe(Some(Epoch::new()), &running);
        assert_eq!(tracker.unread_ids().count(), 0);
    }

    #[test]
    fn presentation_follows_the_unread_flag() {
        let id = SessionId::new();
        let done = info(id, exited(0));
        let unread = present(&done, Some(Unread::Done));
        assert_eq!((unread.label.as_str(), unread.needs_you), ("done", true));
        assert_eq!(unread.tone, Tone::Success);
        let read = present(&done, None);
        assert_eq!((read.label.as_str(), read.needs_you), ("exited", false));
        let failed = present(&info(id, exited(3)), Some(Unread::Failed));
        assert_eq!(
            (failed.label.as_str(), failed.detail.as_str()),
            ("failed", "exit 3")
        );
        assert_eq!(
            present(&info(id, SessionState::Running), None).label,
            "idle"
        );
        assert_eq!(
            ended_text(&exited(3)).as_deref(),
            Some("The process exited with code 3")
        );
    }
}
