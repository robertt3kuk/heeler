//! Session commands: `new`, `ls`, `send`, `stop`, `restart`, `rename`, `history`, and the
//! target resolution they share with `attach`.

use std::collections::BTreeMap;
use std::io::IsTerminal as _;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use heeler_client::Client;
use heeler_core::{SessionId, SessionState, TerminalId};
use heeler_protocol::messages::{
    DaemonStatusInfo, Empty, Request, SessionCreate, SessionInfo, SessionList, SessionListFilter,
    SessionRef, SessionRename, SessionResult, SessionSpec, TerminalAttach, TerminalAttached,
    TerminalHistory, TerminalHistoryRows, TerminalRef, TerminalSize, TerminalSpec,
};
use heeler_protocol::render::{Row, TerminalRender};
use serde_json::Value;
use tokio::time::timeout;

use super::attach::end_message;
use super::{Ctx, print_line};
use crate::args::{HistoryArgs, LsArgs, NewArgs, RenameArgs, SendArgs};
use crate::error::CliError;
use heeler_cli::host;

/// Shortest session id prefix accepted as a target.
const MIN_PREFIX: usize = 4;
/// Scrollback rows requested per `terminal.history` call, so a reply stays well below the
/// protocol's frame limit.
const HISTORY_PAGE: u32 = 1000;
const SNAPSHOT_WAIT: Duration = Duration::from_secs(5);

impl Ctx {
    pub(super) async fn new_session(&self, args: NewArgs) -> Result<(), CliError> {
        let size = if args.attach {
            require_tty()?;
            Some(host_size()?)
        } else {
            None
        };
        // The daemon resolves paths from its own working directory, not ours.
        let cwd = match args.cwd {
            Some(path) => std::path::absolute(path)?,
            None => std::env::current_dir()?,
        };
        let client = self.connect().await?;
        // Subscribe before creating so an instantly exiting command is not missed by `attach`.
        let events = client.events();
        if args.attach {
            let _: heeler_protocol::messages::Subscribed = self
                .call(&client, Request::EventsSubscribe(Empty {}))
                .await?;
        }
        let created: SessionResult = self
            .call(
                &client,
                Request::SessionCreate(SessionCreate {
                    workspace_id: args.workspace,
                    spec: SessionSpec::Terminal(TerminalSpec {
                        command: args.argv,
                        cwd: Some(cwd),
                        env: args.env.into_iter().collect::<BTreeMap<_, _>>(),
                        title: args.title,
                        size,
                    }),
                }),
            )
            .await?;
        self.announce(&created.session, &created)?;
        if let SessionState::Failed { reason } = &created.session.state {
            return Err(CliError::Failed(format!(
                "session {} failed to start: {reason}",
                created.session.id
            )));
        }
        if args.attach && !created.session.state.is_live() {
            // Ended before we could attach (a command that exits at once).
            return print_line(&end_message(&created.session.state));
        }
        if args.attach {
            self.attach_session(client, events, created.session).await
        } else {
            Ok(())
        }
    }

    pub(super) async fn ls(&self, args: LsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let mut list = self.session_list(&client, args.workspace).await?;
        list.sessions.sort_by_key(|s| s.created_at_ms);
        self.print(&list, || session_table(&list.sessions, now_ms()))
    }

    pub(super) async fn send(&self, args: SendArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, &args.target).await?;
        let terminal = live_terminal(&session)?;
        let mut bytes = args.text.into_bytes();
        if args.enter {
            bytes.push(b'\r');
        }
        client
            .send_input(terminal, &bytes)
            .await
            .map_err(|e| self.fail(e))?;
        // Input frames have no response. A round trip on the same connection proves the daemon
        // read them, and keeps the process alive until the socket wrote them out.
        let _: DaemonStatusInfo = self.call(&client, Request::DaemonStatus(Empty {})).await?;
        self.print(
            &serde_json::json!({"session_id": session.id, "bytes": bytes.len()}),
            || format!("sent {} byte(s) to {}", bytes.len(), session.id),
        )
    }

    pub(super) async fn stop(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, target).await?;
        let result: SessionResult = self
            .call(
                &client,
                Request::SessionStop(SessionRef {
                    session_id: session.id,
                }),
            )
            .await?;
        self.print(&result, || format!("stopped {}", result.session.id))
    }

    pub(super) async fn restart(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, target).await?;
        let result: SessionResult = self
            .call(
                &client,
                Request::SessionRestart(SessionRef {
                    session_id: session.id,
                }),
            )
            .await?;
        self.announce(&result.session, &result)
    }

    pub(super) async fn rename(&self, args: RenameArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, &args.target).await?;
        let result: SessionResult = self
            .call(
                &client,
                Request::SessionRename(SessionRename {
                    session_id: session.id,
                    title: args.title,
                }),
            )
            .await?;
        self.print(&result, || {
            format!("renamed {} to {}", result.session.id, result.session.title)
        })
    }

    pub(super) async fn history(&self, args: HistoryArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let session = self.resolve(&client, &args.target).await?;
        let terminal = session
            .terminal_id
            .ok_or_else(|| CliError::Failed(format!("session {} has no terminal", session.id)))?;
        let want = usize::try_from(args.lines).unwrap_or(usize::MAX);

        // Scrollback only holds lines that already left the screen; the current screen comes
        // from a snapshot. Attach without a size so no other client's view is resized.
        let mut frames = client.terminal_frames(terminal);
        let _: TerminalAttached = self
            .call(
                &client,
                Request::TerminalAttach(TerminalAttach {
                    terminal_id: terminal,
                    size: None,
                }),
            )
            .await?;
        let snapshot = match timeout(SNAPSHOT_WAIT, frames.recv()).await {
            Ok(Some(TerminalRender::Snapshot(snapshot))) => snapshot,
            Ok(Some(TerminalRender::Frame(_))) | Ok(None) | Err(_) => {
                return Err(CliError::Failed(
                    "the daemon did not send a screen snapshot".to_owned(),
                ));
            }
        };
        let _: Value = self
            .call(
                &client,
                Request::TerminalDetach(TerminalRef {
                    terminal_id: terminal,
                }),
            )
            .await?;

        let mut screen: Vec<String> = snapshot.lines.iter().map(row_text).collect();
        while screen.last().is_some_and(String::is_empty) {
            screen.pop();
        }
        let missing = want.saturating_sub(screen.len());
        let mut scrollback_rows = Vec::new(); // newest first
        let available = usize::try_from(snapshot.history_len).unwrap_or(usize::MAX);
        let total = missing.min(available);
        while scrollback_rows.len() < total {
            let count = HISTORY_PAGE
                .min(u32::try_from(total - scrollback_rows.len()).unwrap_or(HISTORY_PAGE));
            let from_line = u32::try_from(scrollback_rows.len()).unwrap_or(u32::MAX);
            let page: TerminalHistoryRows = self
                .call(
                    &client,
                    Request::TerminalHistory(TerminalHistory {
                        terminal_id: terminal,
                        from_line,
                        count,
                    }),
                )
                .await?;
            if page.rows.is_empty() {
                break;
            }
            scrollback_rows.extend(page.rows);
        }
        let mut lines: Vec<String> = scrollback_rows.iter().rev().map(row_text).collect();
        lines.extend(screen);
        let start = lines.len().saturating_sub(want);
        let lines = &lines[start..];
        if self.json {
            print_line(&serde_json::json!({"session_id": session.id, "lines": lines}).to_string())
        } else {
            lines.iter().try_for_each(|line| print_line(line))
        }
    }

    // ---- helpers shared with attach -------------------------------------------------------

    pub(super) async fn session_list(
        &self,
        client: &Client,
        workspace_id: Option<heeler_core::WorkspaceId>,
    ) -> Result<SessionList, CliError> {
        self.call(
            client,
            Request::SessionList(SessionListFilter { workspace_id }),
        )
        .await
    }

    pub(super) async fn resolve(
        &self,
        client: &Client,
        target: &str,
    ) -> Result<SessionInfo, CliError> {
        let list = self.session_list(client, None).await?;
        resolve_target(&list.sessions, target).cloned()
    }

    /// `session <id> terminal <tid>` (or the JSON of `value`).
    fn announce(
        &self,
        session: &SessionInfo,
        value: &impl serde::Serialize,
    ) -> Result<(), CliError> {
        self.print(value, || match session.terminal_id {
            Some(terminal) => format!("session {} terminal {terminal}", session.id),
            None => format!("session {}", session.id),
        })
    }
}

/// The terminal of a session that can still take input and be attached live.
pub(super) fn live_terminal(session: &SessionInfo) -> Result<TerminalId, CliError> {
    if !session.state.is_live() {
        return Err(CliError::Failed(format!(
            "session {} is {}; `heeler restart` starts it again, `heeler history` shows its output",
            session.id,
            state_text(&session.state)
        )));
    }
    session
        .terminal_id
        .ok_or_else(|| CliError::Failed(format!("session {} has no terminal", session.id)))
}

pub(super) fn require_tty() -> Result<(), CliError> {
    if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
        Ok(())
    } else {
        Err(CliError::Usage(
            "attaching needs a terminal on stdin and stdout".to_owned(),
        ))
    }
}

pub(super) fn host_size() -> Result<TerminalSize, CliError> {
    let (cols, rows) = host::size()?;
    Ok(terminal_size(cols, rows))
}

pub(super) fn terminal_size(cols: u16, rows: u16) -> TerminalSize {
    TerminalSize {
        cols,
        rows,
        pixel_width: 0,
        pixel_height: 0,
    }
}

/// Picks one session. Tried in order: full session id, `t:<terminal id>`, exact title, session
/// id prefix of at least [`MIN_PREFIX`] characters. A title or prefix matching several sessions
/// is a usage error that lists the candidates; no match is a plain failure.
pub(super) fn resolve_target<'a>(
    sessions: &'a [SessionInfo],
    target: &str,
) -> Result<&'a SessionInfo, CliError> {
    if let Ok(id) = target.parse::<SessionId>()
        && let Some(session) = sessions.iter().find(|s| s.id == id)
    {
        return Ok(session);
    }
    if let Some(number) = target.strip_prefix("t:") {
        let terminal: TerminalId = number.parse().map_err(|_| {
            CliError::Usage(format!("`{target}` is not a terminal id (t:<number>)"))
        })?;
        return sessions
            .iter()
            .find(|s| s.terminal_id == Some(terminal))
            .ok_or_else(|| not_found(target));
    }
    let by_title: Vec<_> = sessions.iter().filter(|s| s.title == target).collect();
    if !by_title.is_empty() {
        return unique(target, by_title);
    }
    if target.len() >= MIN_PREFIX {
        let prefix = target.to_ascii_uppercase();
        let by_prefix: Vec<_> = sessions
            .iter()
            .filter(|s| s.id.to_string().starts_with(&prefix))
            .collect();
        if !by_prefix.is_empty() {
            return unique(target, by_prefix);
        }
    }
    Err(not_found(target))
}

fn unique<'a>(
    target: &str,
    mut matches: Vec<&'a SessionInfo>,
) -> Result<&'a SessionInfo, CliError> {
    match matches.len() {
        1 => Ok(matches.remove(0)),
        n => {
            let mut message = format!("`{target}` matches {n} sessions; use a longer id:");
            for session in matches {
                message.push_str(&format!(
                    "\n  {}  {:<5} {}",
                    session.id,
                    terminal_text(session),
                    session.title
                ));
            }
            Err(CliError::Usage(message))
        }
    }
}

fn not_found(target: &str) -> CliError {
    let hint = if target.len() < MIN_PREFIX && !target.starts_with("t:") {
        format!(" (an id prefix needs at least {MIN_PREFIX} characters)")
    } else {
        String::new()
    };
    CliError::Failed(format!(
        "no session matches `{target}`{hint}; `heeler ls` lists sessions"
    ))
}

fn terminal_text(session: &SessionInfo) -> String {
    session
        .terminal_id
        .map_or_else(|| "-".to_owned(), |t| format!("t:{t}"))
}

pub(super) fn state_text(state: &SessionState) -> String {
    match state {
        SessionState::Starting => "starting".to_owned(),
        SessionState::Running => "running".to_owned(),
        SessionState::Idle => "idle".to_owned(),
        SessionState::WaitingApproval => "waiting approval".to_owned(),
        SessionState::Exited {
            code: Some(code), ..
        } => format!("exited ({code})"),
        SessionState::Exited {
            code: None,
            signal: Some(signal),
        } => format!("exited (signal {signal})"),
        SessionState::Exited { .. } => "exited".to_owned(),
        SessionState::Failed { reason } => format!("failed: {reason}"),
        SessionState::Stopped => "stopped".to_owned(),
    }
}

fn session_table(sessions: &[SessionInfo], now_ms: u64) -> String {
    if sessions.is_empty() {
        return "no sessions".to_owned();
    }
    let rows: Vec<[String; 6]> = sessions
        .iter()
        .map(|s| {
            [
                s.id.to_string(),
                terminal_text(s),
                state_text(&s.state),
                s.title.clone(),
                s.cwd
                    .as_ref()
                    .map_or_else(|| "-".to_owned(), |c| c.display().to_string()),
                age_text(now_ms.saturating_sub(s.created_at_ms) / 1000),
            ]
        })
        .collect();
    let header = ["ID", "TERMINAL", "STATE", "TITLE", "CWD", "AGE"].map(str::to_owned);
    let mut widths = [0; 6];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let mut table = String::new();
    for (index, row) in std::iter::once(&header).chain(&rows).enumerate() {
        if index > 0 {
            table.push('\n');
        }
        let mut line = String::new();
        for (cell, width) in row.iter().zip(widths).take(5) {
            line.push_str(&format!("{cell:<width$}  "));
        }
        line.push_str(&row[5]);
        table.push_str(&line);
    }
    table
}

/// `5s`, `3m`, `2h`, `4d`: one unit, rounded down.
fn age_text(secs: u64) -> String {
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => format!("{}h", secs / 3600),
        _ => format!("{}d", secs / 86_400),
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn row_text(row: &Row) -> String {
    let text: String = row.runs.iter().map(|run| run.text.as_str()).collect();
    text.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use heeler_core::WorkspaceId;

    use super::*;

    fn session(id: &str, terminal: u64, title: &str) -> SessionInfo {
        SessionInfo {
            id: id.parse().unwrap(),
            workspace_id: WorkspaceId::new(),
            kind: heeler_core::SessionKind::Terminal,
            title: title.to_owned(),
            state: SessionState::Running,
            command: vec![],
            cwd: Some(PathBuf::from("/tmp")),
            terminal_id: Some(TerminalId(terminal)),
            created_at_ms: 0,
        }
    }

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";
    const C: &str = "01K0000000000000000000000C";

    fn fixture() -> Vec<SessionInfo> {
        vec![
            session(A, 1, "alpha"),
            session(B, 2, "beta"),
            session(C, 3, "alpha"),
        ]
    }

    #[test]
    fn resolves_full_id_terminal_title_and_prefix() {
        let sessions = fixture();
        assert_eq!(resolve_target(&sessions, A).unwrap().id.to_string(), A);
        assert_eq!(resolve_target(&sessions, "t:2").unwrap().id.to_string(), B);
        assert_eq!(resolve_target(&sessions, "beta").unwrap().id.to_string(), B);
        assert_eq!(
            resolve_target(&sessions, "01k0").unwrap().id.to_string(),
            C,
            "prefixes are case-insensitive"
        );
    }

    #[test]
    fn ambiguous_targets_are_usage_errors_listing_candidates() {
        let sessions = fixture();
        for target in ["01J0", "alpha"] {
            let error = resolve_target(&sessions, target).unwrap_err();
            assert!(matches!(error, CliError::Usage(_)), "{target}: {error:?}");
            assert_eq!(error.exit_code(), 2);
            let text = error.to_string();
            assert!(text.contains(A) && text.contains("t:1"), "{text}");
        }
    }

    #[test]
    fn unmatched_targets_fail_without_guessing() {
        let sessions = fixture();
        for target in ["nope", "t:9", "01J", "gamma"] {
            let error = resolve_target(&sessions, target).unwrap_err();
            assert!(matches!(error, CliError::Failed(_)), "{target}: {error:?}");
        }
        assert!(matches!(
            resolve_target(&sessions, "t:x"),
            Err(CliError::Usage(_))
        ));
        assert!(
            resolve_target(&sessions, "01J")
                .unwrap_err()
                .to_string()
                .contains("at least 4")
        );
    }

    #[test]
    fn ages_round_down_to_one_unit() {
        assert_eq!(age_text(5), "5s");
        assert_eq!(age_text(125), "2m");
        assert_eq!(age_text(7200), "2h");
        assert_eq!(age_text(200_000), "2d");
    }

    #[test]
    fn states_show_exit_codes() {
        assert_eq!(
            state_text(&SessionState::Exited {
                code: Some(3),
                signal: None
            }),
            "exited (3)"
        );
        assert_eq!(
            state_text(&SessionState::Exited {
                code: None,
                signal: Some(9)
            }),
            "exited (signal 9)"
        );
    }
}
