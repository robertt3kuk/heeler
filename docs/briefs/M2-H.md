# M2-H: compact scrollback store (ADR 0013) + B4a/B4b bench split (roadmap M2.7)

Agent: implementer   Depends on: master `a556e0c`+   Branch: slice/M2-H

## Context
Read `AGENTS.md`, ADR 0004, ADR 0011, `docs/perf.md` (§ B2 root cause and follow-up, § B4), `docs/protocol.md` (terminal semantics: `terminal.history` newest-first paging, 5 000 rows / 2 MiB cap, link table), `crates/terminal` (`emulator.rs`, `render.rs`, `terminal.rs`), `xtask/src/bench.rs`.

Prior art (ideas only — Warp is AGPL-3.0, do NOT copy code): Warp keeps the cursor-reachable region in a mutable cell grid and moves rows that scrolled out of reach into `FlatStorage`: one packed UTF-8 byte buffer, a row index (row → byte offset), and styling as interval maps keyed on byte offsets (only where style changes), so resize only rebuilds the row index. Blog: <https://www.warp.dev/blog/block-model-behind-warps-agentic-development-environment>.

## Scope
`crates/terminal` (main work), `crates/daemon` only if wiring changes, `xtask` (bench), `docs/adr/0013-compact-scrollback.md` (new, write it first), `docs/protocol.md` semantics notes, `docs/perf.md` results. No protocol type changes; `heeler-terminal`'s public API stays source-compatible for the daemon.

## Change
1. **ADR 0013** (amends ADR 0004 § scrollback): hot window in alacritty (screen + small margin, e.g. 256 lines) and a per-terminal **cold store** for older rows: packed text bytes + row index (byte offset, wrapped flag, logical-line start) + style spans (fg, bg, flags, underline color, link id) keyed by byte offset, only at changes; newest-first indexing exactly as `terminal.history` uses today; total line bound = `scrollback_lines` exactly (cold rows evicted oldest-first). Settle: spilling after every `feed` (slice large chunks so the engine's history cap is never exceeded before spilling); resize reflows the hot window only, cold rows keep their original width (clients already render rows of any width) — document this behavior; `ESC[3J` (erase scrollback) clears both; alternate screen: no history (unchanged); link ids stay stable (cold store keeps link ids; link table unchanged).
2. **Implementation** in `crates/terminal`: `ScrollbackStore` (module `scrollback`), spill from the engine grid into the store, `history()`/`history_while()`/`history_len()` read across hot + cold transparently, `RenderTracker`/snapshot behavior unchanged (viewport only). Memory: no per-cell allocation in cold rows; amortized growth; eviction frees memory (chunked buffers so evicting old rows actually releases capacity).
3. **Selection/search readiness**: expose an iterator over logical lines (text only) across hot + cold for future search/blocks (M10); test it.
4. **Bench**: B2 must meet ≤ 120 MB at 80 columns; add B2w at 200 columns ≤ 150 MB. Split B4 per ADR 0011 into **B4a** (Heeler path: key handled → frame with the echo submitted for presentation, p95 ≤ 2 ms) and **B4b** (on screen p95 ≤ one display frame + 2 ms using the refresh rate of the screen the window is on, read via the platform and recorded in the report). Update `docs/perf.md` (method + results table, median of 3).

## Acceptance
- Unit/property tests: random output streams (SGR, wide chars, combining, links, wraps, `ESC[3J`, alt screen) through (a) the new emulator and (b) a reference emulator with full alacritty history at the same line bound → `history()` rows identical for every range; eviction keeps exactly `scrollback_lines`; resize: hot window reflows, cold rows unchanged, total count consistent.
- Corpus and existing terminal/daemon/desktop tests pass unchanged (except documented behavior notes).
- `cargo run -p xtask --release -- bench` passes every budget; numbers in `docs/perf.md`; no new `heeler-desktop` crash reports, no orphans.
- `just check` green; committed on `slice/M2-H`.

## Rules
Inline `HEELER_HOME=<temp dir>` on every command that runs heeler/heelerd/heeler-desktop. The user may be running a desktop on `/tmp/hh-g` — never interact with it.

## Report
Summary, ADR decisions, before/after memory and B4a/B4b numbers, files changed, deviations, open questions.
