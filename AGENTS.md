# Heeler — agent and contributor guide

macOS-first Rust agentic development environment: `heelerd` (Tokio daemon, source of truth) + GPUI desktop + CLI over a versioned Unix-socket protocol.

## Resume work (do this first, every session)
1. Read `docs/status.md` → current milestone, active slice, **Next action**, blockers.
2. Read that milestone in `docs/roadmap.md` and the ADRs it references (`docs/adr/`).
3. Follow `docs/process.md` (session start/end protocol, Definition of Done).
4. Before stopping — even mid-slice — update `docs/status.md` with an exact next action.

## Document map
- `docs/vision.md` — target scope, principles, non-goals, performance budgets
- `docs/architecture.md` — components, crates, data flows, state ownership
- `docs/protocol.md` — IPC framing, handshake, sequencing, message catalog
- `docs/roadmap.md` — milestones, slices (checkboxes), risk register
- `docs/process.md` — how we work, gates, testing strategy
- `docs/adr/` — decisions; `docs/spikes/` — evidence behind them

## Invariants (review blockers)
- Daemon owns processes, PTYs, terminal state, sessions, conversations, approvals, credentials, worktrees. Clients only render and send commands (ADR 0001).
- Dependencies point inward: `desktop`/`cli` → `client` → `protocol` → `core`. Nothing depends on `daemon`. Only `desktop` links GPUI (ADR 0003).
- Permissions are enforced in the daemon; UI only collects decisions.
- Secrets only in Keychain via the daemon; never in SQLite, logs, events, or fixtures (ADR 0008).
- Subscription logins are used only through vendor harnesses (Codex, Claude Code); never reuse their tokens.
- Bounded everything: scrollback, event ring, per-client queues. No unbounded channels.
- Protocol changes: additive = minor bump + capability; breaking = major bump + ADR (ADR 0002).
- Hard-to-reverse decision → ADR first; missing evidence → spike outside the repo, report into `docs/spikes/`.

## Conventions
- Rust 1.98.0 (`rust-toolchain.toml`), edition 2024, packages named `heeler-<dir>` under `crates/`, `thiserror` in libraries, `anyhow` only in binaries, `tracing` for logs. New dependencies go into root `[workspace.dependencies]`.
- No `unwrap()`/`expect()` outside tests and proven invariants (comment why); clippy enforces it.
- Conventional commits, scope = crate or `docs`/`adr`.
- Quality gate: `just check` (fmt, clippy `-D warnings`, nextest, cargo-deny). Tools: `brew install just cargo-deny cargo-nextest`.
- Protocol wire changes: bump `PROTOCOL_VERSION`, then `just update-golden`; fixture diffs are reviewed like code.
- Real model/API calls never run in the default gate; use recorded fixtures.

## Parallel work
Split by crate; one worktree per slice (`../heeler-wt/<slice>` on `slice/<slice>`), implemented by the omp `implementer` agent (`.omp/agents/implementer.md`, Claude Sonnet 5.5); one integration owner merges into `master`; only the contract owner edits `core`/`protocol`.

If you are an implementer working from a brief (`docs/briefs/<slice>.md`): stay inside the brief's scope, do not edit `docs/status.md` or `docs/roadmap.md`, report contract problems instead of changing `core`/`protocol`, and finish with `just check` green and a commit on your slice branch.
