//! OSC sniffer: extracts session metadata from the raw byte stream *before* it reaches the VT
//! engine (Alacritty ignores OSC 133 and does not surface OSC 7/9/777).
//!
//! It mirrors the termination rules of the engine's parser (`vte`): an OSC string ends at BEL,
//! at ST (`ESC \`), at any other `ESC` (which starts a new escape sequence) and at CAN/SUB.
//! State carries across calls, so sequences split over arbitrary read boundaries are handled.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use crate::queries::is_color_query;

/// Longest OSC payload that is buffered; longer strings are discarded when they end.
const MAX_OSC_LEN: usize = 8 * 1024;

/// Semantic prompt marks (OSC 133, FinalTerm/iTerm2 shell integration).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptMark {
    /// Prompt start.
    A,
    /// Prompt end / command input start.
    B,
    /// Command output start.
    C,
    /// Command finished, with its exit code when the shell reported one.
    D { exit_code: Option<i32> },
}

/// Out-of-band terminal events for session metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMeta {
    /// OSC 0 / OSC 2. Empty string = title cleared.
    Title(String),
    /// BEL outside a control sequence (reported by the engine).
    Bell,
    /// OSC 9 (`title` empty) and OSC 777;notify.
    Notification { title: String, body: String },
    /// OSC 7 working directory.
    Cwd(PathBuf),
    /// OSC 133 A/B/C/D.
    PromptMark(PromptMark),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ground,
    Escape,
    Osc,
}

#[derive(Debug)]
pub(crate) struct OscSniffer {
    state: State,
    buf: Vec<u8>,
    overflow: bool,
}

impl OscSniffer {
    pub(crate) fn new() -> Self {
        Self {
            state: State::Ground,
            buf: Vec::new(),
            overflow: false,
        }
    }

    /// Scans `bytes`, appending any complete metadata events to `out` and, for every colour
    /// query (OSC 4/10/11/12 with a `?`), the offset just past the byte that ended it to
    /// `color_queries`: the engine has handled the query once it consumed `bytes[..offset]`.
    pub(crate) fn feed(
        &mut self,
        bytes: &[u8],
        out: &mut Vec<TerminalMeta>,
        color_queries: &mut Vec<usize>,
    ) {
        let mut i = 0;
        while i < bytes.len() {
            match self.state {
                State::Ground => {
                    // Fast path: only ESC can leave the ground state.
                    match bytes[i..].iter().position(|&b| b == 0x1b) {
                        Some(offset) => {
                            i += offset + 1;
                            self.state = State::Escape;
                        }
                        None => return,
                    }
                }
                State::Escape => {
                    match bytes[i] {
                        b']' => {
                            self.buf.clear();
                            self.overflow = false;
                            self.state = State::Osc;
                        }
                        0x1b => {}
                        _ => self.state = State::Ground,
                    }
                    i += 1;
                }
                State::Osc => {
                    let b = bytes[i];
                    i += 1;
                    match b {
                        0x07 | 0x18 | 0x1a => {
                            self.finish(out, color_queries, i);
                            self.state = State::Ground;
                        }
                        0x1b => {
                            self.finish(out, color_queries, i);
                            self.state = State::Escape;
                        }
                        0x00..=0x06 | 0x08..=0x17 | 0x19 | 0x1c..=0x1f => {}
                        _ if self.buf.len() < MAX_OSC_LEN => self.buf.push(b),
                        _ => self.overflow = true,
                    }
                }
            }
        }
    }

    /// Completes the OSC string that ended just before offset `end`.
    fn finish(&mut self, out: &mut Vec<TerminalMeta>, color_queries: &mut Vec<usize>, end: usize) {
        if !self.overflow {
            if let Some(meta) = parse_osc(&self.buf) {
                out.push(meta);
            }
            if is_color_query(&self.buf) {
                color_queries.push(end);
            }
        }
        self.buf.clear();
        self.overflow = false;
    }
}

fn parse_osc(payload: &[u8]) -> Option<TerminalMeta> {
    let (ps, pt) = match payload.iter().position(|&b| b == b';') {
        Some(at) => (&payload[..at], &payload[at + 1..]),
        None => (payload, &payload[payload.len()..]),
    };
    match ps {
        b"0" | b"2" => Some(TerminalMeta::Title(
            String::from_utf8_lossy(pt).into_owned(),
        )),
        b"7" => parse_cwd(pt).map(TerminalMeta::Cwd),
        // `9;4;…` is the ConEmu progress report, not a notification.
        b"9" if pt.starts_with(b"4;") => None,
        b"9" => Some(TerminalMeta::Notification {
            title: String::new(),
            body: String::from_utf8_lossy(pt).into_owned(),
        }),
        b"777" => {
            let mut parts = pt.splitn(3, |&b| b == b';');
            if parts.next()? != b"notify" {
                return None;
            }
            let title = String::from_utf8_lossy(parts.next()?).into_owned();
            let body = parts
                .next()
                .map(String::from_utf8_lossy)
                .unwrap_or_default()
                .into_owned();
            Some(TerminalMeta::Notification { title, body })
        }
        b"133" => parse_prompt_mark(pt).map(TerminalMeta::PromptMark),
        _ => None,
    }
}

fn parse_prompt_mark(pt: &[u8]) -> Option<PromptMark> {
    let mut parts = pt.split(|&b| b == b';');
    match parts.next()? {
        b"A" => Some(PromptMark::A),
        b"B" => Some(PromptMark::B),
        b"C" => Some(PromptMark::C),
        b"D" => {
            let exit_code = parts
                .next()
                .and_then(|code| std::str::from_utf8(code).ok())
                .and_then(|code| code.parse().ok());
            Some(PromptMark::D { exit_code })
        }
        _ => None,
    }
}

/// `file://host/path` → percent-decoded path. The host is ignored (shells report their own
/// hostname, which may differ from the daemon's `gethostname` under mDNS/VPN renames).
fn parse_cwd(url: &[u8]) -> Option<PathBuf> {
    let rest = url.strip_prefix(b"file://")?;
    let path = &rest[rest.iter().position(|&b| b == b'/')?..];
    let mut decoded = Vec::with_capacity(path.len());
    let mut i = 0;
    while i < path.len() {
        if path[i] == b'%'
            && let Some(byte) = path.get(i + 1..i + 3).and_then(hex_pair)
        {
            decoded.push(byte);
            i += 3;
            continue;
        }
        decoded.push(path[i]);
        i += 1;
    }
    Some(PathBuf::from(OsString::from_vec(decoded)))
}

fn hex_pair(pair: &[u8]) -> Option<u8> {
    let digit = |b: u8| char::from(b).to_digit(16);
    Some((digit(pair[0])? * 16 + digit(pair[1])?) as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sniff(chunks: &[&[u8]]) -> Vec<TerminalMeta> {
        let mut sniffer = OscSniffer::new();
        let mut out = Vec::new();
        for chunk in chunks {
            sniffer.feed(chunk, &mut out, &mut Vec::new());
        }
        out
    }

    #[test]
    fn title_with_bel_and_st() {
        let got = sniff(&[b"\x1b]0;one\x07x\x1b]2;two\x1b\\"]);
        assert_eq!(
            got,
            [
                TerminalMeta::Title("one".into()),
                TerminalMeta::Title("two".into())
            ]
        );
    }

    #[test]
    fn split_across_every_byte() {
        let stream: &[u8] = b"hi\x1b]7;file://host/tmp/a%20b\x07\x1b]133;D;3\x1b\\\x1b]777;notify;T;B;with;semis\x07";
        let whole = sniff(&[stream]);
        let split: Vec<&[u8]> = stream.chunks(1).collect();
        assert_eq!(sniff(&split), whole);
        assert_eq!(
            whole,
            [
                TerminalMeta::Cwd(PathBuf::from("/tmp/a b")),
                TerminalMeta::PromptMark(PromptMark::D { exit_code: Some(3) }),
                TerminalMeta::Notification {
                    title: "T".into(),
                    body: "B;with;semis".into()
                },
            ]
        );
    }

    #[test]
    fn prompt_marks_and_notifications() {
        let got =
            sniff(&[b"\x1b]133;A\x07\x1b]133;B\x07\x1b]133;C\x07\x1b]133;D\x07\x1b]9;hello\x07"]);
        assert_eq!(
            got,
            [
                TerminalMeta::PromptMark(PromptMark::A),
                TerminalMeta::PromptMark(PromptMark::B),
                TerminalMeta::PromptMark(PromptMark::C),
                TerminalMeta::PromptMark(PromptMark::D { exit_code: None }),
                TerminalMeta::Notification {
                    title: String::new(),
                    body: "hello".into()
                },
            ]
        );
    }

    #[test]
    fn progress_and_unknown_osc_ignored() {
        assert!(
            sniff(&[b"\x1b]9;4;1;50\x07\x1b]52;c;aGk=\x07\x1b]4;1;rgb:00/00/00\x07"]).is_empty()
        );
    }

    #[test]
    fn esc_inside_osc_ends_it_and_starts_a_new_sequence() {
        let got = sniff(&[b"\x1b]0;a\x1b]0;b\x07"]);
        assert_eq!(
            got,
            [
                TerminalMeta::Title("a".into()),
                TerminalMeta::Title("b".into())
            ]
        );
    }

    #[test]
    fn oversized_osc_is_dropped_and_parser_recovers() {
        let mut stream = b"\x1b]0;".to_vec();
        stream.extend(std::iter::repeat_n(b'x', MAX_OSC_LEN + 10));
        stream.extend_from_slice(b"\x07\x1b]0;ok\x07");
        assert_eq!(sniff(&[&stream]), [TerminalMeta::Title("ok".into())]);
    }
}
