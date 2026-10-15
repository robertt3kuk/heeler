//! Tooling around recorded Codex transcripts: [`proxy`] records a real session, [`scrub`] makes
//! the recording committable, [`replay`] is the fake `codex app-server` the tests run against.
//! `docs/agents.md` describes the workflow; the `staffy-codex-fixture` binary is the front end.

pub mod proxy;
pub mod replay;
pub mod scrub;
