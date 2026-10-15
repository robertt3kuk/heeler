# M2-F: performance budgets — measure, fix, and keep measuring (roadmap M2.6)

Agent: implementer   Depends on: master `9889781`+   Branch: slice/M2-F

## Context
Read `AGENTS.md`, `docs/vision.md` § Budgets (binding numbers), `docs/design.md`, ADR 0004 (ack pacing), `crates/daemon/examples/flood.rs` and `crates/terminal/examples/flood.rs` (existing benches), `crates/desktop` (app, `sessions/`, `workbench/terminals.rs`), `crates/terminal_view` (`view.rs`, `element.rs`, `shape.rs`, `shared.rs`).

## Scope
New `xtask` crate (workspace member `xtask/`, package `xtask`, not shipped) for benchmark orchestration; instrumentation and fixes in `desktop`, `terminal_view`, `client`, `daemon`, `terminal` as needed; `justfile` recipe `bench`; `docs/perf.md` (new) with results. Do not change `protocol`/`core` without reporting first. Never take screenshots or trigger permission prompts; never use the orchestrator's `STAFFY_HOME`s — use temp dirs.

## Budgets to measure (vision.md)
| # | Metric | Budget | How |
|---|---|---|---|
| B1 | Idle desktop RSS, 1 window, 4 terminals | ≤ 150 MB | launch `staffy-desktop` with a seeded `desktop-state.json` (4 terminal panes), wait 10 s idle, sample RSS of the desktop pid |
| B2 | Idle daemon RSS, 10 sessions, 10k-line scrollback each | ≤ 120 MB | 10 sessions each printing 10 000 lines then idling; sample `staffyd` RSS |
| B3 | Idle CPU (daemon + desktop) | ≈ 0 % (≤ 0.5 % avg over 30 s) | `ps`/`proc_pidinfo` CPU time delta over 30 s with B1+B2 running |
| B4 | Keystroke → glyph on screen | ≤ 16 ms p95 | in-app tracing (below) over 500 keystrokes into a `cat` session |
| B5 | 200 MB output flood | UI stays interactive; daemon memory bounded | flood a visible terminal while sending keystrokes to another; latency p95 of the other ≤ 50 ms; daemon RSS growth ≤ 50 MB |
| B6 | Cold start to first terminal prompt (daemon running) | ≤ 500 ms | process start → first paint of a terminal snapshot that contains the prompt, via tracing |

## Instrumentation
- `STAFFY_TRACE_LATENCY=1` env enables lightweight tracing in the desktop (no cost when off): timestamp when a key event is handled and its bytes are sent; timestamp when a frame that changes the cursor row/text after that input is first painted (correlate by sequence: the next painted frame after input whose dirty rows include the cursor row). Emit JSON lines to `STAFFY_TRACE_FILE`. Also `startup` events (process start, window shown, connection connected, first snapshot painted).
- Key injection for B4/B5 without OS events: a debug-only action dispatched through GPUI (`STAFFY_BENCH_INPUT` env: the desktop reads a script of keystrokes and dispatches them through the normal key handling path at a fixed rate). It must exercise the same code path as real typing (input handler → encoding → `TerminalHandle::input`).

## Harness
`cargo run -p xtask --release -- bench [--only B1,B4] [--json]`: builds release binaries, creates a temp `STAFFY_HOME`, starts `staffyd`, seeds sessions/layout, launches `staffy-desktop` (the window will appear on screen; that is expected and allowed — no screenshots), runs each scenario, tears everything down (no orphans), prints a table and writes `target/bench/<timestamp>.json`. `just bench` wraps it.

## Fix what misses
Profile with `sample <pid>` / Instruments CLI (`xctrace record --template 'Time Profiler'` if available without prompts; otherwise `sample`). Typical suspects: re-shaping unchanged rows, per-frame allocations in the element, overview previews painting at full rate, settings/watch polling, idle timers (cursor blink must not keep CPU busy: use a timer only while focused, and only repaint the cursor cell), daemon render coalescing. Every fix needs a before/after number.

## Acceptance
- `docs/perf.md`: method, machine, table with each budget, measured value (median of 3 runs), pass/fail, and fixes made with before/after.
- All budgets pass, or a miss is explained with a measured root cause and a concrete follow-up (report it; do not silently lower budgets).
- `just check` green (bench is not part of the gate). Committed on `slice/M2-F`.

## Report
Summary, table, fixes, files changed, deviations, open questions.
