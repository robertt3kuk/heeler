//! Turns a raw recording of an ACP agent into a committable fixture: no account data, no machine
//! names, no home directories, session and tool call ids replaced by stable counters.
//!
//! Like the Codex scrubber it is idempotent (scrubbing a fixture changes nothing) and refuses
//! a recording that still looks private afterwards.

use std::collections::HashMap;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use super::scrub::ScrubError;

const FIXTURE_CWD: &str = "/tmp/hutan-fixture";
const FIXTURE_HOME: &str = "/tmp/fixture-home";
/// Keys whose string value is an opaque id of the agent.
const SESSION_KEYS: &[&str] = &["sessionId"];
/// Keys dropped wherever they appear: timestamps and machine details.
const DROPPED_KEYS: &[&str] = &["updatedAt", "createdAt", "t"];

fn re(pattern: &str) -> Regex {
    // The patterns are literals of this file and tested.
    #[allow(clippy::expect_used)]
    Regex::new(pattern).expect("a valid regex")
}

static TEMP_DIR: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?:/private)?/var/folders/[^/\s]+/[^/\s]+/T/[A-Za-z0-9._-]+|/tmp/hutan-(?:rec|m8a-rec)[A-Za-z0-9._-]*",
    )
});
static HOME_DIR: LazyLock<Regex> =
    LazyLock::new(|| re(r"/Users/[A-Za-z0-9._-]+|/home/[a-z][A-Za-z0-9._-]*"));
static EMAIL: LazyLock<Regex> =
    LazyLock::new(|| re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"));
static FIXTURE_SESSION: LazyLock<Regex> = LazyLock::new(|| re(r"^sess_fixture_(\d+)$"));

/// Strings that mean the recording still holds something private.
static LEAKS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        ("a home directory", re(r"/Users/|/home/[a-z]")),
        (
            "an email address",
            re(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}"),
        ),
        ("an API key", re(r"\bsk-[A-Za-z0-9_-]{10,}")),
        (
            "a bearer or JWT token",
            re(r"\beyJ[A-Za-z0-9_-]{10,}|[Bb]earer [A-Za-z0-9._-]{16,}"),
        ),
        ("a macOS temp directory", re(r"/var/folders/")),
    ]
});

#[derive(Debug, Default)]
struct Renamer {
    sessions: HashMap<String, String>,
    next: u64,
}

impl Renamer {
    fn seeded(lines: &[Value]) -> Self {
        let mut renamer = Self::default();
        let text: String = lines.iter().map(|l| format!("{l}\n")).collect();
        for word in text.split(|c: char| !c.is_ascii_alphanumeric() && c != '_') {
            if let Some(n) = FIXTURE_SESSION
                .captures(word)
                .and_then(|c| c[1].parse::<u64>().ok())
            {
                renamer.next = renamer.next.max(n);
                renamer.sessions.insert(word.to_owned(), word.to_owned());
            }
        }
        renamer
    }

    fn session(&mut self, original: &str) -> String {
        if let Some(known) = self.sessions.get(original) {
            return known.clone();
        }
        self.next += 1;
        let renamed = format!("sess_fixture_{}", self.next);
        self.sessions.insert(original.to_owned(), renamed.clone());
        renamed
    }

    fn string(&self, text: &str) -> String {
        let text = TEMP_DIR.replace_all(text, FIXTURE_CWD);
        let text = HOME_DIR.replace_all(&text, FIXTURE_HOME);
        let text = EMAIL.replace_all(&text, "user@example.com");
        // A session id that appears inside other text (a path of the agent's own store).
        let mut text = text.into_owned();
        for (from, to) in &self.sessions {
            if from != to && !from.is_empty() {
                text = text.replace(from, to);
            }
        }
        text
    }

    fn value(&mut self, value: Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.string(&text)),
            Value::Array(items) => Value::Array(items.into_iter().map(|v| self.value(v)).collect()),
            Value::Object(map) => {
                let mut out = Map::new();
                for (key, inner) in map {
                    if DROPPED_KEYS.contains(&key.as_str()) {
                        continue;
                    }
                    let inner = match (&inner, SESSION_KEYS.contains(&key.as_str())) {
                        (Value::String(id), true) => Value::String(self.session(id)),
                        _ => self.value(inner),
                    };
                    out.insert(key, inner);
                }
                Value::Object(out)
            }
            other => other,
        }
    }
}

/// Scrubs a raw transcript (one `{"dir", "msg"}` object per line).
pub fn scrub(raw: &str) -> Result<String, ScrubError> {
    let mut lines = Vec::new();
    for (index, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(|source| ScrubError::Json {
            line: index + 1,
            source,
        })?;
        lines.push(value);
    }
    let mut renamer = Renamer::seeded(&lines);
    // Session ids are learned from every message first, so a path that mentions one before
    // the agent names it is still renamed.
    for line in &lines {
        learn(&mut renamer, line);
    }
    let mut out = String::new();
    for (index, line) in lines.into_iter().enumerate() {
        let scrubbed = renamer.value(line);
        check_leaks(&scrubbed, index + 1)?;
        out.push_str(&scrubbed.to_string());
        out.push('\n');
    }
    Ok(out)
}

fn learn(renamer: &mut Renamer, value: &Value) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                match inner {
                    Value::String(id) if SESSION_KEYS.contains(&key.as_str()) => {
                        renamer.session(id);
                    }
                    other => learn(renamer, other),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|v| learn(renamer, v)),
        _ => {}
    }
}

fn check_leaks(value: &Value, line: usize) -> Result<(), ScrubError> {
    match value {
        Value::String(text) => {
            for (what, pattern) in LEAKS.iter() {
                // The one address the scrubber writes itself.
                let text = text.replace("user@example.com", "");
                if let Some(found) = pattern.find(&text) {
                    return Err(ScrubError::Leak {
                        line,
                        what,
                        sample: found.as_str().to_owned(),
                    });
                }
            }
            Ok(())
        }
        Value::Array(items) => items.iter().try_for_each(|v| check_leaks(v, line)),
        Value::Object(map) => map.values().try_for_each(|v| check_leaks(v, line)),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = r#"
{"dir":"out","t":1.0,"msg":{"jsonrpc":"2.0","id":1,"method":"session/new","params":{"cwd":"/private/var/folders/f8/abc/T/hutan-rec-xyz","mcpServers":[]}}}
{"dir":"in","t":1.1,"msg":{"jsonrpc":"2.0","id":1,"result":{"sessionId":"0f4c7d52a1b3"}}}
{"dir":"in","t":1.2,"msg":{"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"0f4c7d52a1b3","update":{"sessionUpdate":"session_info_update","title":"mail me@example.org","updatedAt":"2026-10-15T10:00:00Z","path":"/Users/x/.omp/agent/sessions/0f4c7d52a1b3.jsonl"}}}}
"#;

    #[test]
    fn scrubbing_removes_account_machine_and_path_details() {
        let out = scrub(RAW).unwrap();
        assert!(!out.contains("/Users/x"), "{out}");
        assert!(!out.contains("/var/folders"), "{out}");
        assert!(!out.contains("me@example.org"), "{out}");
        assert!(!out.contains("0f4c7d52a1b3"), "{out}");
        assert!(!out.contains("updatedAt"), "{out}");
        assert!(!out.contains("\"t\""), "{out}");
        assert!(out.contains("sess_fixture_1"), "{out}");
        assert!(out.contains(FIXTURE_CWD), "{out}");
        assert!(out.contains(FIXTURE_HOME), "{out}");
    }

    #[test]
    fn scrubbing_twice_changes_nothing() {
        let once = scrub(RAW).unwrap();
        assert_eq!(scrub(&once).unwrap(), once);
    }

    #[test]
    fn a_token_is_refused() {
        let raw = r#"{"dir":"in","msg":{"result":{"note":"Bearer abcdefghijklmnopqrstuvwxyz"}}}"#;
        assert!(matches!(scrub(raw), Err(ScrubError::Leak { .. })));
    }
}
