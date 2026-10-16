//! The last lines of a terminal session as plain text: what `hutan history` prints and what the
//! MCP tool `session_output` returns.

use std::time::Duration;

use hutan_client::{Client, ClientError};
use hutan_core::TerminalId;
use hutan_protocol::messages::{
    Empty, Request, TerminalAttach, TerminalAttached, TerminalHistory, TerminalHistoryRows,
    TerminalRef,
};
use hutan_protocol::render::{Row, TerminalRender};
use hutan_protocol::{ErrorBody, ErrorCode};
use tokio::time::timeout;

/// Scrollback rows requested per `terminal.history` call, so a reply stays well below the
/// protocol's frame limit.
const HISTORY_PAGE: u32 = 1000;
const SNAPSHOT_WAIT: Duration = Duration::from_secs(5);

/// The text of a row without its trailing blanks.
pub fn row_text(row: &Row) -> String {
    let text: String = row.runs.iter().map(|run| run.text.as_str()).collect();
    text.trim_end().to_owned()
}

/// Up to `want` lines of `terminal`, oldest first: scrollback followed by the screen.
///
/// Scrollback only holds lines that already left the screen; the current screen comes from a
/// snapshot. Attaches without a size so no other client's view is resized.
pub async fn terminal_tail(
    client: &Client,
    terminal: TerminalId,
    want: usize,
) -> Result<Vec<String>, ClientError> {
    let mut frames = client.terminal_frames(terminal);
    let _: TerminalAttached = client
        .call(Request::TerminalAttach(TerminalAttach {
            terminal_id: terminal,
            size: None,
        }))
        .await?;
    let snapshot = match timeout(SNAPSHOT_WAIT, frames.recv()).await {
        Ok(Some(TerminalRender::Snapshot(snapshot))) => snapshot,
        Ok(Some(TerminalRender::Frame(_))) | Ok(None) | Err(_) => {
            return Err(ClientError::Remote(ErrorBody::new(
                ErrorCode::Internal,
                "the daemon did not send a screen snapshot",
            )));
        }
    };
    let _: Empty = client
        .call(Request::TerminalDetach(TerminalRef {
            terminal_id: terminal,
        }))
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
        let count =
            HISTORY_PAGE.min(u32::try_from(total - scrollback_rows.len()).unwrap_or(HISTORY_PAGE));
        let from_line = u32::try_from(scrollback_rows.len()).unwrap_or(u32::MAX);
        let page: TerminalHistoryRows = client
            .call(Request::TerminalHistory(TerminalHistory {
                terminal_id: terminal,
                from_line,
                count,
            }))
            .await?;
        if page.rows.is_empty() {
            break;
        }
        scrollback_rows.extend(page.rows);
    }
    let mut lines: Vec<String> = scrollback_rows.iter().rev().map(row_text).collect();
    lines.extend(screen);
    let start = lines.len().saturating_sub(want);
    Ok(lines.split_off(start))
}
