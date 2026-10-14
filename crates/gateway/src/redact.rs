//! Removing credentials from text that came from (or is about) a provider before it can reach a
//! log, an event or the journal: providers echo parts of a rejected key in their error messages
//! ("Incorrect API key provided: sk-proj-****abcd").

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

use crate::secret::Secret;

pub const REDACTED: &str = "[REDACTED]";

/// Well-known key shapes, including the masked forms providers print.
static SHAPES: LazyLock<Regex> = LazyLock::new(|| {
    // The pattern is a constant covered by the unit tests.
    #[allow(clippy::expect_used)]
    Regex::new(
        r"(?i)\b(?:sk|pk|rk|gsk|xai|key)[-_][A-Za-z0-9_*.\-]{6,}|\bBearer\s+[A-Za-z0-9._~+/=*\-]{8,}",
    )
    .expect("constant redaction pattern")
});

/// Knows the credential a request used and scrubs it (and anything shaped like a key) from text.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    secret: Option<Secret>,
}

impl Redactor {
    pub fn new(secret: Option<Secret>) -> Self {
        Self { secret }
    }

    pub fn apply<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        if let Some(secret) = &self.secret
            && secret.expose().len() >= 4
            && out.contains(secret.expose())
        {
            out = Cow::Owned(out.replace(secret.expose(), REDACTED));
        }
        if SHAPES.is_match(&out) {
            out = Cow::Owned(SHAPES.replace_all(&out, REDACTED).into_owned());
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_exact_secret_and_key_shapes_are_removed() {
        let redactor = Redactor::new(Some(Secret::new("hunter2-very-secret")));
        let out = redactor
            .apply("bad key hunter2-very-secret; also sk-proj-****abcd and Bearer abcdefgh12345");
        assert!(!out.contains("hunter2"), "{out}");
        assert!(!out.contains("sk-proj"), "{out}");
        assert!(!out.contains("abcdefgh"), "{out}");
        assert!(out.starts_with("bad key [REDACTED]"), "{out}");
    }

    #[test]
    fn ordinary_text_is_untouched() {
        let text = "model `gpt-4o-mini` not found; the task-force key is on the table";
        assert_eq!(Redactor::default().apply(text), text);
    }
}
