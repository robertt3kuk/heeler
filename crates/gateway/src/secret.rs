//! A secret string that cannot be printed by accident.

use std::fmt;

/// An API key. `Debug` shows nothing of it, there is no `Display` and no `Serialize`: the only
/// way to the text is [`Secret::expose`], which the adapters call when they build a header.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn new(secret: impl Into<String>) -> Self {
        Self(secret.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}
