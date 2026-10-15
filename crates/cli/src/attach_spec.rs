//! `staffy ask --attach <spec>`: the command-line spelling of an assistant attachment.
//!
//! ```text
//! block:<terminal>:<block>        the output of one terminal block (`staffy api terminal.blocks`)
//! recent:<terminal>:<lines>       the last lines of a terminal (at most 500)
//! agent:<session>[:<turns>]       the last turns (default 5, at most 20) of a Staffy chat, or of
//!                                 a Claude Code, Codex or OMP session found by `staffy search`
//! file:<path>[:<first>-<last>]    a file under the workspace's folders, or some of its lines
//! search:<query>                  the best history hits (5) for the words; quotes are optional
//! ```

use staffy_core::TerminalId;
use staffy_protocol::assistant::{Attachment, LineRange};

/// Turns of an `agent:` attachment that names none.
const DEFAULT_TURNS: u32 = 5;
/// Hits of a `search:` attachment.
const DEFAULT_HITS: u32 = 5;

fn number<T: std::str::FromStr>(text: &str, what: &str) -> Result<T, String> {
    text.trim()
        .parse()
        .map_err(|_| format!("{what} must be a number, got `{text}`"))
}

/// Parses one `--attach` value.
pub fn parse_attachment(spec: &str) -> Result<Attachment, String> {
    let Some((kind, rest)) = spec.split_once(':') else {
        return Err(format!(
            "expected KIND:VALUE (block, recent, agent, file or search), got `{spec}`"
        ));
    };
    match kind {
        "block" => {
            let (terminal, block) = rest
                .split_once(':')
                .ok_or("expected block:<terminal>:<block>")?;
            Ok(Attachment::ShellBlock {
                terminal_id: TerminalId(number(terminal, "the terminal id")?),
                block_id: number(block, "the block id")?,
            })
        }
        "recent" => {
            let (terminal, lines) = rest
                .split_once(':')
                .ok_or("expected recent:<terminal>:<lines>")?;
            Ok(Attachment::ShellRecent {
                terminal_id: TerminalId(number(terminal, "the terminal id")?),
                lines: number(lines, "the line count")?,
            })
        }
        "agent" => {
            let (session, turns) = match rest.rsplit_once(':') {
                Some((session, turns)) => (session, number(turns, "the turn count")?),
                None => (rest, DEFAULT_TURNS),
            };
            if session.is_empty() {
                return Err("expected agent:<session>[:<turns>]".to_owned());
            }
            Ok(Attachment::AgentTranscript {
                session_id: session.to_owned(),
                last_turns: turns,
            })
        }
        "file" => {
            // A trailing `:<first>-<last>` is a line range; anything else belongs to the path.
            let (path, range) = match rest.rsplit_once(':') {
                Some((path, range)) => match range.split_once('-') {
                    Some((start, end)) => match (start.parse::<u32>(), end.parse::<u32>()) {
                        (Ok(start), Ok(end)) => (path, Some(LineRange { start, end })),
                        _ => (rest, None),
                    },
                    None => (rest, None),
                },
                None => (rest, None),
            };
            if path.is_empty() {
                return Err("expected file:<path>[:<first>-<last>]".to_owned());
            }
            Ok(Attachment::File {
                path: path.to_owned(),
                range,
            })
        }
        "search" => {
            let query = rest.trim();
            let query = query
                .strip_prefix('"')
                .and_then(|q| q.strip_suffix('"'))
                .unwrap_or(query);
            if query.trim().is_empty() {
                return Err("expected search:<query>".to_owned());
            }
            Ok(Attachment::Search {
                query: query.to_owned(),
                limit: DEFAULT_HITS,
            })
        }
        other => Err(format!(
            "unknown attachment kind `{other}` (block, recent, agent, file or search)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_parses() {
        assert_eq!(
            parse_attachment("block:3:12").unwrap(),
            Attachment::ShellBlock {
                terminal_id: TerminalId(3),
                block_id: 12
            }
        );
        assert_eq!(
            parse_attachment("recent:3:80").unwrap(),
            Attachment::ShellRecent {
                terminal_id: TerminalId(3),
                lines: 80
            }
        );
        assert_eq!(
            parse_attachment("agent:01K7F3B5N2Q9W8E4R6T1Y0V3KM").unwrap(),
            Attachment::AgentTranscript {
                session_id: "01K7F3B5N2Q9W8E4R6T1Y0V3KM".into(),
                last_turns: 5
            }
        );
        assert_eq!(
            parse_attachment("agent:5f3e1c1a-8d51:3").unwrap(),
            Attachment::AgentTranscript {
                session_id: "5f3e1c1a-8d51".into(),
                last_turns: 3
            }
        );
        assert_eq!(
            parse_attachment("file:src/main.rs").unwrap(),
            Attachment::File {
                path: "src/main.rs".into(),
                range: None
            }
        );
        assert_eq!(
            parse_attachment("file:src/main.rs:10-40").unwrap(),
            Attachment::File {
                path: "src/main.rs".into(),
                range: Some(LineRange { start: 10, end: 40 })
            }
        );
        assert_eq!(
            parse_attachment("file:/tmp/odd:name").unwrap(),
            Attachment::File {
                path: "/tmp/odd:name".into(),
                range: None
            }
        );
        assert_eq!(
            parse_attachment("search:\"scheduler stall\"").unwrap(),
            Attachment::Search {
                query: "scheduler stall".into(),
                limit: 5
            }
        );
        assert_eq!(
            parse_attachment("search:cache").unwrap(),
            Attachment::Search {
                query: "cache".into(),
                limit: 5
            }
        );
    }

    #[test]
    fn malformed_specs_say_what_was_expected() {
        for bad in [
            "block",
            "block:3",
            "block:x:1",
            "recent:3:many",
            "agent:",
            "agent:s:many",
            "file:",
            "search:",
            "search:\"\"",
            "nonsense:1",
        ] {
            assert!(parse_attachment(bad).is_err(), "{bad}");
        }
    }
}
