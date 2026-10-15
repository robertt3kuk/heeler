# 0008. Credentials and provider auth policy

- Status: accepted
- Date: 2026-10-13
- Milestone: M5

## Context
We support API keys, local models, OAuth where providers officially support third-party clients, and existing subscriptions. Subscription logins (ChatGPT for Codex, Claude for Claude Code) are licensed to the vendor's own clients; reusing those tokens as API credentials violates terms and breaks without notice.

## Decision
- Secrets live only in macOS Keychain, written/read by the daemon (`security` crate, `security-framework`). SQLite stores provider config and a Keychain reference, never the secret. Clients send a secret once via `provider.set_credential`; it is never echoed back or logged.
- **Direct model chat** (our gateway) uses: API keys, local endpoints, and OAuth flows that the provider documents for third-party apps.
- **Subscription access** is used only by running the vendor's harness (Codex app-server/CLI with ChatGPT login, Claude Code with Claude login, etc.) as an agent session. Staffy never extracts or forwards those tokens.
- Agent processes receive credentials only via the vendor's own login state or explicit env injection configured by the user per provider.
- Logs and events pass through a redaction layer for known secret shapes.

## Consequences
- "Use my ChatGPT plan in direct chat" is answered by "open a Codex harness chat", not by the gateway.
- Keychain prompts appear for the daemon binary identity → the daemon must be code-signed with a stable identity (see ADR 0007).
