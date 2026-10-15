# M1-B: `terminal` crate — PTY, engine, render tracking, conformance corpus (roadmap M1.3 + M0.4)

Agent: implementer   Depends on: M0 (commit 6c99867)   Branch: slice/M1-B

## Context
Read `AGENTS.md`, then ADR 0004 (decision is binding), `docs/spikes/2026-10-13-terminal-engine.md` (measured behavior and pitfalls of `alacritty_terminal` 0.26 and `portable-pty` 0.9), `docs/protocol.md` § Terminal render types, `crates/protocol/src/render.rs` (the DTOs you must produce). Pinned deps already in root `[workspace.dependencies]`: `alacritty_terminal = "=0.26.0"`, `portable-pty = "=0.9.0"`.

## Scope
Create `crates/terminal` (package `staffy-terminal`, library only). Add needed deps to root `[workspace.dependencies]`. Do **not** edit `crates/protocol` or `crates/core`; if the render DTOs cannot express something, stop and describe it in the report. No Tokio runtime dependency in the public API beyond `tokio::sync` types.

## Contract
```rust
pub struct TerminalConfig { pub argv: Vec<String> /* empty = $SHELL -l */, pub cwd: Option<PathBuf>, pub env: BTreeMap<String,String>, pub size: TerminalSize, pub scrollback_lines: usize /* default 10_000 */ }
pub struct Terminal { /* Send + Sync handle, cheap to clone or Arc-wrapped */ }
impl Terminal {
    pub fn spawn(cfg: TerminalConfig) -> Result<Self, TerminalError>;
    pub fn write_input(&self, bytes: &[u8]) -> Result<(), TerminalError>;
    pub fn resize(&self, size: TerminalSize) -> Result<(), TerminalError>;   // PTY TIOCSWINSZ + engine resize
    pub fn snapshot(&self, tracker: &mut RenderTracker) -> TerminalSnapshot; // resets tracker to this state
    pub fn frame(&self, tracker: &mut RenderTracker) -> Option<TerminalFrame>; // None if nothing changed since tracker's state
    pub fn history(&self, from_line: u32, count: u32) -> Vec<Row>;
    pub fn damage(&self) -> tokio::sync::watch::Receiver<u64>;              // generation counter, coalescing
    pub fn metadata(&self) -> tokio::sync::broadcast::Receiver<TerminalMeta>; // Title, Bell, Notification{title,body}, Cwd(PathBuf), PromptMark(A|B|C|D)
    pub fn pid(&self) -> Option<u32>;
    pub fn terminate(&self, grace: Duration) -> Result<(), TerminalError>;  // SIGHUP, then SIGKILL after grace
    pub async fn wait_exit(&self) -> ExitStatus;                            // code / signal; reaped exactly once
}
pub struct RenderTracker { /* per-client last-sent rows, cursor, modes, title, frame counter */ }
```
Behavior requirements:
- One blocking reader thread per PTY → OSC sniffer → `alacritty_terminal::Term::advance` → bump damage generation. Engine `Event::PtyWrite` replies (DA/DSR/DECRQM/kitty query) are written back to the PTY through the same single writer used by `write_input`.
- `Config { kitty_keyboard: true, scrolling_history: cfg.scrollback_lines, .. }`. DEC 2026 synchronized output honoured: while a sync batch is open, no damage is published; the 150 ms `sync_timeout` must be serviced (timer thread or reader poll) so a never-closed batch still renders.
- OSC sniffer runs on raw bytes before the engine, tolerant of sequences split across reads and of both BEL and ST terminators; extracts OSC 0/2 (title), 7 (cwd `file://host/path`), 9 and 777 (notifications), 133 A/B/C/D (prompt marks); bell (BEL outside sequences) from the engine event.
- Env: inherit the daemon env, then `cfg.env`, then force `TERM=xterm-256color`, `COLORTERM=truecolor`, `TERM_PROGRAM=staffy`. Login shell from `$SHELL`, falling back to `/bin/zsh`.
- Drop the PTY slave right after spawn; spawns serialized behind a global mutex (non-atomic CLOEXEC window, see spike). No fd leaks: after `terminate` + `wait_exit` the process fd count returns to its pre-spawn value.
- Render conversion: engine grid → `Row { runs }` merging adjacent cells with identical style; wide char = one run char + `WIDE` flag, spacer cells skipped; combining chars appended to their base; hyperlinks mapped to a per-terminal link table (ids stable for the terminal lifetime); cursor shape/visibility/blinking and `Modes` mapped from engine `TermMode`; palette overrides from OSC 4/10/11/12 tracked.
- `RenderTracker` diff: compare current viewport rows to the tracker's last-sent rows; emit only changed rows; `scrolled` may stay 0 in this slice (document it). A tracker created fresh always gets a full snapshot.
- Memory: scrollback bounded by `scrollback_lines`; no per-terminal unbounded buffers.

### Conformance corpus (M0.4)
- `crates/terminal/tests/corpus/feature/*.vt` hand-written byte files + expected outcomes covering: SGR 16/256/truecolor and underline styles, alt screen enter/exit restores primary, CUP addressing, scroll regions, wide chars + emoji + combining, OSC 8 links, OSC 133 marks, DEC 2026 open/closed, bracketed paste/kitty/DA/DSR/DECRQM replies (assert exact reply bytes, see spike § Corpus and replies).
- A recorder example (`cargo run -p staffy-terminal --example record -- <name> -- <argv…>`) that runs a command in a PTY at 80×24, saves raw output to `tests/corpus/recorded/<name>.raw`, and the final grid as text to `<name>.grid`. Record and commit: `zsh -f -c 'printf …; ls'`, `vim -u NONE -n` editing a line then `:q!` (scripted input), `htop` for ~1 s then `q` if installed (skip with a note otherwise). Do not record agent TUIs that need logins or tokens; note them as open items.
- Test replays every recorded `.raw` through a fresh engine and compares the grid text to `.grid`.

## Acceptance
- Tests: corpus tests above; PTY test spawning `/bin/sh`, `echo` round-trip via `write_input` visible in snapshot; resize test observes `stty size` change; vim test reaches alt screen and back; fd-count test; replies test asserts exact DA/DSR bytes were written back (use `cat -v` style echo or a loopback check in the child).
- Bench example `cargo run --release -p staffy-terminal --example flood` feeding 200 MB of mixed output through the engine path (no PTY) prints MB/s and peak RSS; record the numbers in the report.
- `just check` green. No `unwrap/expect` outside tests. Any `unsafe` (e.g. `libc` fd ops) must carry a `// SAFETY:` comment and a crate-level `#![allow(unsafe_code)]` scoped to the module that needs it.

## Out of scope
Daemon integration, protocol messages, attach flow (integration slice M1.4), CLI rendering (M1-C), persistence.

## Report
Summary, files changed, commands run with results (tests, bench numbers), deviations from the contract, open questions. Commit on `slice/M1-B` (conventional message, scope `terminal`). Do not edit `docs/status.md` or `docs/roadmap.md`.
