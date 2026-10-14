//! Best-effort removal of obvious secrets before any text reaches the index (ADR 0008: secrets
//! never go to SQLite). Patterns: well-known token shapes (`sk-…`, `ghp_…`, `AKIA…`, JWTs, Slack
//! and Google keys), PEM private keys, `Bearer …` / `Authorization:` credentials, passwords in
//! URLs, and `name=value` / `"name": "value"` pairs whose name looks secret (the same name
//! fragments the daemon uses to keep environment variables out of the session database).
//!
//! This is a heuristic, not a guarantee: a secret in an unusual shape is indexed as written.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::{Captures, Regex};

pub const REDACTED: &str = "[REDACTED]";

/// Name fragments that make a variable or key look secret; mirrors the daemon's
/// `SECRET_NAME_PARTS` (`TOKEN`, `SECRET`, …) with the separator variants seen in config files.
const NAME_PATTERN: &str = r"(?i)\b[\w.-]*(?:token|secret|password|passwd|credential|api[_-]?key|private[_-]?key|access[_-]?key)[\w.-]*";

struct Patterns {
    pem: Regex,
    tokens: Regex,
    bearer: Regex,
    authorization: Regex,
    url_credentials: Regex,
    assignment: Regex,
}

static PATTERNS: LazyLock<Patterns> = LazyLock::new(|| Patterns {
    pem: compile(
        r"(?s)-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----.*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|\z)",
    ),
    tokens: compile(
        r"\bsk-[A-Za-z0-9][A-Za-z0-9_-]{15,}|\bgh[pousr]_[A-Za-z0-9]{20,}|\bgithub_pat_[A-Za-z0-9_]{20,}|\b(?:AKIA|ASIA)[0-9A-Z]{16}\b|\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}|\bAIza[0-9A-Za-z_-]{35}|\bxox[abposr]-[0-9A-Za-z-]{10,}",
    ),
    bearer: compile(r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/=-]{16,}"),
    authorization: compile(r"(?i)\b(authorization\s*[:=]\s*)(?:basic|bearer|token)\s+[^\s\x22']+"),
    url_credentials: compile(r"\b([A-Za-z][A-Za-z0-9+.-]*://[^/\s:@]+:)[^@\s/]+@"),
    assignment: compile(&format!(
        r#"({NAME_PATTERN}["']?\s*[:=]\s*)("[^"\n]{{4,}}"|'[^'\n]{{4,}}'|[^\s"',;)}}\]]{{6,}})"#
    )),
});

#[allow(clippy::expect_used)] // The patterns are constants covered by the unit tests.
fn compile(pattern: &str) -> Regex {
    Regex::new(pattern).expect("constant redaction pattern")
}

/// `text` with everything that looks like a secret replaced by `[REDACTED]`.
pub fn redact(text: &str) -> Cow<'_, str> {
    let p = &*PATTERNS;
    let mut out = Cow::Borrowed(text);
    replace(&mut out, &p.pem, |_| REDACTED.to_owned());
    replace(&mut out, &p.tokens, |_| REDACTED.to_owned());
    replace(&mut out, &p.authorization, |c| {
        format!("{}{REDACTED}", &c[1])
    });
    replace(&mut out, &p.bearer, |c| format!("{}{REDACTED}", &c[1]));
    replace(&mut out, &p.url_credentials, |c| {
        format!("{}{REDACTED}@", &c[1])
    });
    replace(&mut out, &p.assignment, |c| {
        let name = c[1].to_ascii_lowercase();
        let name = name.trim_end_matches(|ch: char| !ch.is_alphanumeric());
        // Counts and limits (`max_tokens`, `token_count`) are not secrets.
        let is_count = name
            .rsplit(['_', '-', '.'])
            .next()
            .is_some_and(|last| matches!(last, "tokens" | "count" | "limit" | "budget" | "usage"));
        if is_count {
            c[0].to_owned()
        } else {
            format!("{}{REDACTED}", &c[1])
        }
    });
    out
}

fn replace(text: &mut Cow<'_, str>, regex: &Regex, with: impl Fn(&Captures<'_>) -> String) {
    if !regex.is_match(text) {
        return;
    }
    let replaced = regex
        .replace_all(text, |c: &Captures<'_>| with(c))
        .into_owned();
    *text = Cow::Owned(replaced);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clean(text: &str) -> String {
        redact(text).into_owned()
    }

    #[test]
    fn well_known_token_shapes_are_removed() {
        let secrets = [
            "sk-proj-abcdefghijklmnop1234567890",
            "sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUV",
            "ghp_abcdefghijklmnopqrstuvwxyz0123456789",
            "github_pat_11ABCDEFG0abcdefghijkl_mnopqrstuv",
            "AKIAIOSFODNN7EXAMPLE",
            "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0NTY3ODkwIn0.dBjftJeZ4CVPmB92K27uhbUJU1p1r",
            "xoxb-123456789012-abcdefghijkl",
            "AIzaSyA-abcdefghijklmnopqrstuvwxyz01234",
        ];
        for secret in secrets {
            let out = clean(&format!("use {secret} for the call"));
            assert!(!out.contains(secret), "{secret} survived: {out}");
            assert!(out.contains(REDACTED), "{out}");
            assert!(
                out.starts_with("use ") && out.ends_with(" for the call"),
                "{out}"
            );
        }
    }

    #[test]
    fn private_key_blocks_are_removed_whole() {
        let text = "before\n-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXk\nAAAAB3Nza\n-----END OPENSSH PRIVATE KEY-----\nafter";
        assert_eq!(clean(text), "before\n[REDACTED]\nafter");
        // A block cut off by the end of the text (a chunk or a truncated output) goes too.
        assert_eq!(
            clean("x\n-----BEGIN RSA PRIVATE KEY-----\nMIIEow"),
            "x\n[REDACTED]"
        );
    }

    #[test]
    fn bearer_and_authorization_credentials_keep_their_label() {
        assert_eq!(
            clean("curl -H 'Authorization: Bearer abcdef0123456789abcdef' https://x"),
            "curl -H 'Authorization: [REDACTED]' https://x"
        );
        assert_eq!(
            clean("header is Bearer abcdef0123456789abcdef.xyz ok"),
            "header is Bearer [REDACTED] ok"
        );
        assert_eq!(
            clean("Authorization: Basic dXNlcjpwYXNzd29yZA=="),
            "Authorization: [REDACTED]"
        );
        // A short word after "bearer" is prose.
        assert_eq!(clean("the bearer of bad news"), "the bearer of bad news");
    }

    #[test]
    fn url_passwords_are_removed() {
        assert_eq!(
            clean("git clone https://user:hunter2hunter2@example.com/repo.git"),
            "git clone https://user:[REDACTED]@example.com/repo.git"
        );
        assert_eq!(
            clean("see https://example.com/a:b"),
            "see https://example.com/a:b"
        );
    }

    #[test]
    fn secret_looking_assignments_lose_their_value() {
        assert_eq!(
            clean("export GITHUB_TOKEN=ghx_notreallyatoken12345 && run"),
            "export GITHUB_TOKEN=[REDACTED] && run"
        );
        assert_eq!(
            clean(r#"{"api_key": "abcd1234efgh", "name": "x"}"#),
            r#"{"api_key": [REDACTED], "name": "x"}"#
        );
        assert_eq!(clean("DB_PASSWORD='p@ss w0rd!'"), "DB_PASSWORD=[REDACTED]");
        assert_eq!(
            clean("aws_secret_access_key = wJalrXUtnFEMI/K7MDENG"),
            "aws_secret_access_key = [REDACTED]"
        );
        assert_eq!(
            clean("password: correct-horse-battery"),
            "password: [REDACTED]"
        );
    }

    #[test]
    fn ordinary_text_is_untouched() {
        for text in [
            "cargo test -p daemon --locked",
            "let max_tokens = 100000;",
            "token_count: 1234567",
            "the secret to good tests is speed",
            "SSH_AUTH_SOCK=/private/tmp/com.apple.launchd.x/Listeners",
            "sk-short",
            "src/index.rs:42: fn parse_line(&self) -> Option<Entry>",
        ] {
            assert_eq!(clean(text), text);
        }
    }
}
