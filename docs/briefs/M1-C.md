# M1-C: `hutan` CLI and host-terminal renderer (roadmap M1.6, part 1)

Agent: implementer   Depends on: M1-A merged (`hutan-client`)   Branch: slice/M1-C

## Context
Read `AGENTS.md`, then `docs/protocol.md`, ADR 0002, ADR 0004 (§ Stream model: the CLI renders frames into the host terminal, tmux-style, no VT engine of its own), `crates/protocol/src/render.rs`, `crates/client/src/lib.rs`.

## Scope
Create `crates/cli` (package `hutan-cli`, binary `hutan`). Add deps to root `[workspace.dependencies]` (`clap` derive, `crossterm` for raw mode/size, `vt100` as **dev-dependency only** for renderer tests). Do not edit other crates; report needed changes instead.

## Contract
Commands (all support `--json` for machine output; human output otherwise):
- `hutan daemon start` — start `hutand` (found next to the `hutan` binary, else on `PATH`) detached; wait for the socket; idempotent.
- `hutan daemon status` / `hutan daemon stop [--stop-sessions]`.
- `hutan ws ls`, `hutan ws new <name> [--root PATH]…`, `hutan ws rename <id> <name>`, `hutan ws close <id>`.
- `hutan api <method> [<json-params>]` — raw request, prints the result JSON or the error body (exit 1).
- `hutan events [--follow]` — print events as JSON lines.
- Global: `--socket PATH` (else `HUTAN_SOCKET`, else default path from ADR 0002 / `HUTAN_HOME`).
- Exit codes: 0 ok, 1 remote error, 2 usage, 3 daemon unreachable.

Library module `hutan_cli::render` (used by `attach` in the integration slice):
```rust
pub struct HostRenderer { /* tracks what the host terminal currently shows */ }
impl HostRenderer {
    pub fn new(host_cols: u16, host_rows: u16) -> Self;
    pub fn apply_snapshot(&mut self, s: &TerminalSnapshot, out: &mut Vec<u8>);   // full repaint
    pub fn apply_frame(&mut self, f: &TerminalFrame, out: &mut Vec<u8>);         // only dirty rows, cursor, modes
    pub fn resize_host(&mut self, cols: u16, rows: u16, out: &mut Vec<u8>);
    pub fn enter(&self, out: &mut Vec<u8>); pub fn leave(&self, out: &mut Vec<u8>); // alt screen, modes, cursor restore
}
pub struct DetachDetector { /* prefix ctrl+\ (0x1c), then `d` detaches; prefix twice sends one literal 0x1c */ }
```
Renderer requirements: SGR for default/indexed/rgb colors and every `CellFlags` style (undercurl `4:3`, underline color `58:2::r:g:b`), wide runs occupy two columns, OSC 8 for links, cursor position/shape (`DECSCUSR`)/visibility, mirror app modes the host must honour for correct input (bracketed paste `?2004`, app cursor `?1`, mouse modes `?1000/1002/1003` + `?1006`, focus `?1004`, kitty keyboard push/pop `CSI > flags u` / `CSI < u`). Synchronized-update wrap each frame (`CSI ?2026 h … l`). Clip when host is smaller than the remote terminal; pad when larger.

## Acceptance
- Renderer tests: feed `apply_snapshot` + `apply_frame` output into a `vt100::Parser` sized like the host and assert screen contents, colors, attributes, cursor position, and that only dirty rows are rewritten (byte count bound).
- Detach tests: sequences split across reads, double prefix, prefix then other key forwards both bytes.
- CLI tests against a real `hutand` on a temp `HUTAN_HOME` (use `hutan-daemon` lib from M1-A in dev-deps): `daemon status --json`, `ws new` + `ws ls --json`, `api daemon.status`, error exit codes.
- `just check` green.

## Out of scope
`new`, `ls` (sessions), `attach`, `send` wiring — integration slice M1.4/M1.5 after M1-B. No GPUI.

## Report
Summary, files changed, commands run with results, open questions. Commit on `slice/M1-C` (conventional message, scope `cli`). Do not edit `docs/status.md` or `docs/roadmap.md`.
