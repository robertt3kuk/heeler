//! Harness accounts (protocol 0.17, capability `accounts`): DTOs of `account.*` and the
//! `account.changed` event (`docs/protocol.md` § Accounts, ADR 0015).
//!
//! An account is a login the user made *in a harness* (OMP, Codex, Claude Code): Hutan lists
//! what each harness reports about it, and sends the user to the harness's own login flow in a
//! terminal session. Hutan never performs the login, never sees a token and never shows an
//! e-mail address: a provider, a label and the plan are all there is.

use serde::{Deserialize, Serialize};

use crate::messages::SessionInfo;

/// Harness ids the daemon knows for accounts.
pub const HARNESS_OMP: &str = "omp";
pub const HARNESS_CLAUDE: &str = "claude";
pub const HARNESS_CODEX: &str = "codex";

/// Whether a harness has a usable login for a provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountStatus {
    LoggedIn,
    LoggedOut,
    /// The harness could not say (not installed, did not answer); see `detail`.
    Unknown,
}

/// One login of a harness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountInfo {
    /// `omp`, `claude` or `codex`.
    pub harness: String,
    /// The model provider the login is for (`anthropic`, `openai-codex`, ...).
    pub provider: String,
    /// What to call it: the harness's own name for the account kind (`ChatGPT`, `API key`,
    /// `Claude subscription`), never an e-mail address.
    pub label: String,
    /// The subscription plan when the harness reports one (`plus`, `max`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    pub status: AccountStatus,
    /// Why the status is `unknown`, or what the user can do (`install ...`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Result of `account.list`: per harness the accounts it reports, in the order omp, claude,
/// codex. A harness that is not installed, or that did not answer, is one entry with status
/// `unknown` and a `detail`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountList {
    pub accounts: Vec<AccountInfo>,
}

/// `account.login`: start the harness's own login in an interactive terminal session. The
/// result carries that session; the user completes the login in it. `account.changed` follows
/// when the session ends.
///
/// `provider` is the provider to log in to (`omp login anthropic`); absent lets the harness
/// ask. Errors: `bad_request` (unknown harness, a provider the harness cannot take),
/// `provider_error` when the harness's program cannot be found (the message says what to
/// install or which `daemon.toml` key to set).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLogin {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// `account.logout`: like `account.login`, for the harness's own logout. A harness that has no
/// logout command (OMP: `/logout` inside its interface) gets a terminal session running the
/// harness, and `instructions` says what to type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountLogout {
    pub harness: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
}

/// Result of `account.login` and `account.logout`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccountSession {
    /// The terminal session running the harness's flow.
    pub session: SessionInfo,
    /// What the user still has to do in it, when the harness does not do it by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
}
