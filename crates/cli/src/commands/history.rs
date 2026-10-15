//! `hutan search` and `hutan index …`: the session history index (roadmap M17).

use std::io::IsTerminal as _;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use hutan_client::Client;
use hutan_protocol::history::{
    HistoryHit, HistoryHits, HistorySearch, HistorySetEnabled, HistorySource, HistoryStatusInfo,
};
use hutan_protocol::messages::{Empty, Request};
use serde_json::Value;

use super::Ctx;
use crate::args::{IndexCommand, NewArgs, SearchArgs, SourceArg};
use crate::error::CliError;

impl Ctx {
    /// A connection to a daemon that speaks the `history.*` requests.
    async fn connect_history(&self) -> Result<Client, CliError> {
        let client = self.connect().await?;
        if !client.supports_history() {
            return Err(CliError::Failed(
                "this hutand does not offer history search (protocol 0.8); \
                 restart it with `hutan daemon restart`"
                    .to_owned(),
            ));
        }
        Ok(client)
    }

    pub(super) async fn search(&self, args: SearchArgs) -> Result<(), CliError> {
        let client = self.connect_history().await?;
        let request = HistorySearch {
            query: args.query.join(" "),
            limit: args.limit,
            sources: args.source.iter().map(|s| source_of(*s)).collect(),
            roles: Vec::new(),
            project: None,
            since_ms: args.since.map(|age| now_ms().saturating_sub(age)),
            until_ms: None,
        };
        let found: HistoryHits = self.call(&client, Request::HistorySearch(request)).await?;
        if let Some(number) = args.resume {
            drop(client);
            return self.resume_hit(&found.hits, number).await;
        }
        self.print(&found, || {
            hits_text(&found.hits, std::io::stdout().is_terminal())
        })
    }

    /// Opens hit `number` (1-based) in a new session: its working directory, its resume command.
    async fn resume_hit(&self, hits: &[HistoryHit], number: u32) -> Result<(), CliError> {
        let hit = usize::try_from(number)
            .ok()
            .and_then(|n| hits.get(n.checked_sub(1)?))
            .ok_or_else(|| {
                CliError::Failed(format!(
                    "no hit {number}: the search found {} session(s)",
                    hits.len()
                ))
            })?;
        let Some(argv) = hit.resume_argv.clone() else {
            return Err(CliError::Failed(format!(
                "hit {number} ({} session {}) cannot be resumed",
                hit.source.as_str(),
                hit.session_key
            )));
        };
        // The agent's recorded directory may be gone (a deleted worktree); the session then
        // starts where the command was run.
        let cwd = match hit.cwd.as_deref().map(PathBuf::from) {
            Some(dir) if dir.is_dir() => Some(dir),
            Some(dir) => {
                eprintln!(
                    "hutan: {} no longer exists; resuming in the current directory",
                    dir.display()
                );
                None
            }
            None => None,
        };
        let title = hit
            .title
            .as_deref()
            .map_or_else(|| argv.join(" "), |title| format!("resume: {title}"));
        let attach = std::io::stdin().is_terminal() && std::io::stdout().is_terminal();
        self.new_session(NewArgs {
            workspace: None,
            cwd,
            title: Some(title),
            env: Vec::new(),
            attach,
            argv,
        })
        .await
    }

    pub(super) async fn index(&self, command: IndexCommand) -> Result<(), CliError> {
        match command {
            IndexCommand::Status => {
                let client = self.connect_history().await?;
                let info: HistoryStatusInfo =
                    self.call(&client, Request::HistoryStatus(Empty {})).await?;
                self.print(&info, || status_text(&info))
            }
            IndexCommand::Enable | IndexCommand::Disable => {
                let enabled = matches!(command, IndexCommand::Enable);
                // Enabling is the one history command that may start the daemon.
                let (client, _) = self.ensure_daemon().await?;
                if !client.supports_history() {
                    return Err(CliError::Failed(
                        "this hutand does not offer history search (protocol 0.8); \
                         restart it with `hutan daemon restart`"
                            .to_owned(),
                    ));
                }
                let info: HistoryStatusInfo = self
                    .call(
                        &client,
                        Request::HistorySetEnabled(HistorySetEnabled { enabled }),
                    )
                    .await?;
                self.print(&info, || status_text(&info))
            }
            IndexCommand::Clear => {
                let client = self.connect_history().await?;
                let _: Value = self.call(&client, Request::HistoryClear(Empty {})).await?;
                self.print(&serde_json::json!({"cleared": true}), || {
                    "history index cleared".to_owned()
                })
            }
            IndexCommand::Rebuild => {
                let client = self.connect_history().await?;
                let _: Value = self
                    .call(&client, Request::HistoryRebuild(Empty {}))
                    .await?;
                self.print(&serde_json::json!({"rebuilding": true}), || {
                    "rebuilding the history index in the background; `hutan index status` shows progress"
                        .to_owned()
                })
            }
        }
    }
}

fn source_of(source: SourceArg) -> HistorySource {
    match source {
        SourceArg::Hutan => HistorySource::Hutan,
        SourceArg::Claude => HistorySource::Claude,
        SourceArg::Codex => HistorySource::Codex,
        SourceArg::Omp => HistorySource::Omp,
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// `2026-10-15 09:00` in UTC.
pub(crate) fn format_utc(ms: u64) -> String {
    let secs = ms / 1000;
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (hour, minute) = (rest / 3600, rest % 3600 / 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = i64::try_from(days).unwrap_or(0) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// The hit list for people: one block per session, matches in bold on a terminal.
pub(crate) fn hits_text(hits: &[HistoryHit], color: bool) -> String {
    if hits.is_empty() {
        return "no matches".to_owned();
    }
    let mut out = String::new();
    for (index, hit) in hits.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        let when = hit.ts_ms.map_or_else(|| "-".to_owned(), format_utc);
        let title = hit.title.as_deref().unwrap_or("(untitled)");
        let more = if hit.matches > 1 {
            format!(", {} matches", hit.matches)
        } else {
            String::new()
        };
        out.push_str(&format!(
            "{:>2}  {:<6} {when}  {title}  ({}{more})\n",
            index + 1,
            hit.source.as_str(),
            hit.role.as_str(),
        ));
        if let Some(cwd) = &hit.cwd {
            out.push_str(&format!("    {cwd}\n"));
        }
        out.push_str(&format!("    {}\n", snippet_text(&hit.snippet, color)));
        if let Some(argv) = &hit.resume_argv {
            out.push_str(&format!("    resume: {}\n", argv.join(" ")));
        }
    }
    out.truncate(out.trim_end().len());
    out
}

/// One line; the daemon's `«match»` markers become bold on a terminal and stay as they are
/// otherwise. Control characters in indexed text never reach the terminal.
fn snippet_text(snippet: &str, color: bool) -> String {
    let mut out = String::with_capacity(snippet.len());
    for c in snippet.chars() {
        match c {
            '«' if color => out.push_str("\x1b[1m"),
            '»' if color => out.push_str("\x1b[0m"),
            c if c.is_control() => out.push(' '),
            c => out.push(c),
        }
    }
    out
}

fn status_text(info: &HistoryStatusInfo) -> String {
    let mut out = if info.enabled {
        "history index: enabled".to_owned()
    } else {
        "history index: disabled (turn it on with `hutan index enable`)".to_owned()
    };
    for source in &info.sources {
        let last = source
            .last_indexed_ms
            .map_or_else(|| "never".to_owned(), format_utc);
        out.push_str(&format!(
            "\n  {:<7} {:>6} file(s) {:>9} row(s)  last {last}{}",
            source.name,
            source.files,
            source.rows,
            if source.errors > 0 {
                format!("  {} unreadable line(s)", source.errors)
            } else {
                String::new()
            }
        ));
    }
    out.push_str(&format!(
        "\n  database {:.1} MiB",
        info.db_bytes as f64 / (1024.0 * 1024.0)
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_protocol::history::HistoryRole;

    fn hit() -> HistoryHit {
        HistoryHit {
            source: HistorySource::Claude,
            session_key: "s1".into(),
            title: Some("Scheduler stall".into()),
            cwd: Some("/work/app".into()),
            role: HistoryRole::User,
            ts_ms: Some(1_790_571_682_983),
            snippet: "why does the «quokka»\nscheduler stall".into(),
            score: 4.2,
            matches: 3,
            session_id: None,
            resume_argv: Some(vec!["claude".into(), "--resume".into(), "s1".into()]),
        }
    }

    #[test]
    fn utc_dates_format() {
        assert_eq!(format_utc(0), "1970-01-01 00:00");
        assert_eq!(format_utc(1_790_571_682_983), "2026-09-28 05:01");
        assert_eq!(format_utc(1_709_208_000_500), "2024-02-29 12:00");
    }

    #[test]
    fn hits_render_plain_and_with_color() {
        let plain = hits_text(&[hit()], false);
        assert_eq!(
            plain,
            " 1  claude 2026-09-28 05:01  Scheduler stall  (user, 3 matches)\n    /work/app\n    why does the «quokka» scheduler stall\n    resume: claude --resume s1"
        );
        let color = hits_text(&[hit()], true);
        assert!(color.contains("\x1b[1mquokka\x1b[0m"), "{color:?}");
        assert_eq!(hits_text(&[], false), "no matches");
    }
}
