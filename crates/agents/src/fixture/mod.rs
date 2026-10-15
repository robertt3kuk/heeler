//! Tooling around recorded transcripts: [`proxy`] records a real session, [`scrub`] (Codex) and
//! [`acp_scrub`] (ACP agents) make the recording committable, [`replay`] is the fake agent the
//! tests run against (`codex app-server`, `omp acp`). `docs/agents.md` describes the workflow;
//! the `hutan-codex-fixture` and `hutan-acp-fixture` binaries are the front ends.

pub mod acp_scrub;
pub mod proxy;
pub mod replay;
pub mod scrub;
