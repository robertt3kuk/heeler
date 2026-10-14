# Performance budgets: method, results, fixes

Budgets are the ones in `docs/vision.md` § Budgets (unchanged here: a miss is reported, never
adjusted away). Numbers below are from `cargo run -p xtask --release -- bench` (`just bench`),
median of 3 runs, release binaries, on:

| | |
|---|---|
| Machine | Apple M4 (MacBook Air, 24 GB), macOS 26.6.2, `rustc 1.98.0` |
| Displays | built-in 60 Hz Liquid Retina, external 100 Hz; the window opens on whichever macOS picks |
| Session | normal desktop session; the bench window is on screen and active (`b4_window_active = 1` in every B4 run) |

## Results

| # | Metric | Budget | Median of 3 | Runs | Result |
|---|---|---|---|---|---|
| B1 | Idle desktop RSS, 1 window, 4 terminals | ≤ 150 MB | **102.7 MB** | 102.7, 102.9, 102.7 | pass |
| B2 | Idle daemon RSS, 10 sessions × 10k-line scrollback | ≤ 120 MB | **254.6 MB** | 254.6, 254.3, 254.6 | **miss** (below) |
| B3 | Idle CPU, daemon + desktop, 30 s | ≤ 0.5 % | **0.46 %** | 0.44, 0.52, 0.46 | pass, at the limit (below) |
| B4 | Keystroke → glyph on screen, p95 over 500 keys | ≤ 16 ms | **18.0 ms** | 18.0, 17.4, 18.1 | **miss** (below) |
| B5 | 200 MB+ flood: p95 key latency in a second terminal | ≤ 50 ms | **16.9 ms** | 16.2, 16.9, 16.9 | pass |
| B5 | 200 MB+ flood: daemon RSS growth | ≤ 50 MB | **13.6 MB** | 13.6, 13.6, 13.6 | pass |
| B6 | Cold start → first terminal paint with its prompt | ≤ 500 ms | **213 ms** | 220, 186, 213 | pass |

Other numbers behind the table (medians): B1 physical footprint 68.8 MB; daemon with the 10
sessions before the desktop attached 208 MB; B3 desktop 0.46 %, daemon 0.00 %; B4 median latency
10.1 ms; B5 median 9.0 ms, 245 keystrokes measured while the flood ran (12.2 s); B6 timeline
(ms after spawn): `main` 15, window opened 142, first frame 159, connected 171, first terminal text
painted 213.

`xtask bench` exits non-zero while a budget is missed, so the two misses below keep `just bench`
red until they are fixed or the budgets are amended by ADR. The gate (`just check`) does not run
it.

## Method

`cargo run -p xtask --release -- bench [--only B1,B4] [--runs N] [--json] [--sample]
[--key-interval-ms N]` builds `heelerd`, `heeler-desktop` and `heeler` in release, then for every
scenario and run: creates a temporary `HEELER_HOME`, starts `heelerd`, seeds sessions and a
`desktop-state.json`, launches the desktop (the window appears, no screenshots), measures, and
tears down. The report goes to `target/bench/<unix-time>.json` (all runs, every stage metric).

- **B1/B2/B3 (one scenario).** 10 sessions each print 12 000 coloured 80-column lines (the
  10 000-line scrollback overflows) and idle in `cat`. Daemon RSS is sampled; the desktop opens a
  seeded 2×2 layout of 4 of them; after 10 s idle the desktop and daemon are sampled
  (`proc_pid_rusage`: resident size, physical footprint, user+system CPU time, idle wakeups), and
  again after 30 s. B3 is the CPU time delta of both processes over those 30 s.
- **B4.** One `cat`-like session (`printf 'bench> '; exec cat`) in the only pane. The desktop types
  500 keys at one key per 100 ms plus a deterministic 0–17 ms jitter (`HEELER_BENCH_INPUT`, below).
  Latency is the in-app trace: key event handled → first paint that follows the daemon frame
  replacing the cursor row or moving the cursor. The jitter is needed: a strictly periodic injector
  locks onto one phase of the display period and measures that phase for the whole run.
- **B5.** Two panes: a typing session and a flooded one. The flood is six back-to-back
  `head -c 150000000 /dev/urandom | base64` (about 1.2 GB; a single 203 MB round takes only ~2 s
  and is too short to type through). 600 keys at 20 keys/s (+ jitter) are typed into the first
  pane; latency is taken only for keys that were painted while the flood ran. Daemon growth is
  peak RSS minus RSS before the flood.
- **B6.** Daemon running with 4 prompt sessions; `heeler-desktop` is spawned with the seeded
  layout. Cold start is the wall clock from just before the spawn to the end of the first paint of
  a terminal whose snapshot has text (the prompt).

**Instrumentation** (no cost when off: one atomic load):

- `HEELER_TRACE_LATENCY=1` and `HEELER_TRACE_FILE=<path>` enable JSON-lines tracing
  (`terminal_view::trace`): `startup` events (`main`, `window_opened`, `first_frame`, `connected`,
  `first_terminal_painted`), `key` events with the stages `key_to_sent`, `sent_to_echo`,
  `echo_to_paint` (and its first part `echo_to_prepaint`: waiting for the display tick), and once a
  second a `paints` count.
- `HEELER_BENCH_INPUT=<script.json>` types `{"interval_ms", "start_delay_ms", "jitter_ms",
  "keys": ["a", "enter", …]}` through `Window::dispatch_keystroke`: the real dispatch tree, the
  terminal view's key handler, key encoding and `TerminalHandle::input`. It starts after the first
  terminal painted text.
- `--sample` runs `sample(1)` on the desktop and daemon during B3/B4 (`target/bench/sample-*.txt`).
  Symbolised profiles come from `CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=1`
  builds.

**Teardown is graceful and checked.** The desktop is stopped with SIGTERM (it saves its layout and
exits 0, leaving the daemon running); the daemon with `heeler daemon stop --stop-sessions`. The run
fails if either had to be SIGKILLed, died of a signal, or left a process behind (descendants of the
daemon included). Verified across three full runs: no stray `heelerd`/`heeler-desktop`, no new
`heeler-desktop-*.ips` crash report.

## Fixes (before → after, same harness)

1. **Idle CPU: cursor blink (B3).** Before: 1.58 % (1.57, 1.60, 1.58), ~30 wakeups/s. A `sample`
   of the idle desktop attributed it to whole-window repaints (≈4.5 ms each) twice a second for
   the focused cursor, plus one 530 ms timer per terminal view, focused or not. After: the timer
   exists only while the focused view's cursor blinks and stops ~5 s after the last activity
   (typing, output, focus), leaving a solid cursor (Alacritty's `blinking_timeout` behaviour);
   unfocused views schedule nothing. **0.46 %** (0.44, 0.52, 0.46). Test: the cursor stops blinking
   when idle and typing resumes it.
2. **Typing under load: ack when painted (B4/B5 sensitivity).** The terminal view acked a frame
   one display tick after painting it (a `next_frame` callback), so after every paint the daemon
   (credit 1, ADR 0004) had no credit for a whole frame and echoes of fast typing queued behind the
   ack. Now the ack goes out with the paint; frames are still painted at most once per tick, so a
   flood is still paced by the display, and daemon memory stays bounded (RSS growth unchanged,
   13.5 MB).

   | scenario | before p95 (median of 3) | after p95 |
   |---|---|---|
   | B4 at 33 keys/s (`--key-interval-ms 30`) | 32.3 ms (p50 22.1) | **18.4 ms** (p50 11.2) |
   | B5 at 40 keys/s (`--key-interval-ms 25`) | 61.2 ms (runs 61.3, 32.0, 61.2) | **17.2 ms** |
   | B4 at 10 keys/s (default) | 18.8 ms | 18.0 ms (no real change) |
   | B5 at 20 keys/s (default) | 16.6 ms | 16.9 ms (no real change) |

   Test: `frames_are_acked_when_they_are_painted`.
3. **Tried, no measurable effect, reverted:** skipping the redraw that every keystroke caused
   before the echo arrives (B4 at 10 keys/s 18.8 → 18.3 ms, within noise; at 33 keys/s unchanged
   at 32 ms until fix 2).

## Misses

### B2: daemon RSS 254.6 MB against 120 MB

Root cause (measured): scrollback lives in `alacritty_terminal`'s grid, which stores every cell,
blank or not, in 24 bytes. A 10 000-line history of an 80-column session costs
10 000 × 80 × 24 B ≈ 19.2 MB; measured on a fresh daemon: 3.2 MB empty, then about 20.5 MB for each session printing 12 000 lines
(`heeler new` ×4: 24.2, 44.5, 64.7, 85.2 MB).
Ten sessions are 208 MB before any client attaches. Attaching the desktop adds 47 MB more because
its panes are wider than 80 columns and the grid (history included) is reflowed to the pane width:
the cost grows with terminal width (10 000 × 200 × 24 B = 48 MB per session at 200 columns, computed).
The same rows as styled runs are tiny: the 10 000 history rows of this test encode to 2.0 runs and
64 text bytes per row; at about 90 bytes per row in a compact binary encoding (estimate: text
plus ~12 bytes of style per run) that is ~0.9 MB per session instead of 19.2 MB (about 20× less).

Follow-up (needs an ADR amending 0004; not done here because it changes where scrollback lives, how
it is reflowed on resize, and the exactness of the line bound): keep only a small hot window of
history (≈ 1–2 screens beyond the viewport) in the engine and spill older rows into a compact
per-terminal cold store (rows encoded as runs, newest-first indexing as today) that `terminal.history`
reads through. Points the ADR must settle: spilling after every `feed` needs the engine's history
cap to exceed the lines one read chunk can scroll (bounded by slicing the chunk); resize reflows the
hot window only (cold rows keep their width; the client already renders rows of any width);
`ESC[3J` and alternate-screen hiding apply to both parts. Acceptance: B2 ≤ 120 MB at 80 columns and
≤ 150 MB at 200 columns, `just bench --only B2`.

### B4: keystroke → glyph p95 18.0 ms against 16 ms

Where the time goes (medians): key event → bytes sent 0.02 ms; sent → echo frame applied
0.4 ms (p95 0.6); applied → start of the paint that shows it 9.2 ms (p95 16.3); paint itself
≈ 0.5 ms. So everything except the wait for the display tick costs about 1 ms. The tick period
observed while typing is ~17 ms, and an echo arrives at a uniformly random phase, so the p95 of
the wait alone is ≈ 0.95 × 17 ms ≈ 16 ms before the paint is counted: with drawing locked to the
display's vsync (GPUI on macOS draws only inside `CVDisplayLink` callbacks and has no on-demand
frame path at the pinned revision), p95 ≤ 16 ms is not reachable at ~60 Hz. The median is
10.1 ms, the maximum 22.8 ms. During the flood the same machine paints at ~80
frames/s, so the cadence follows the display and load [INFERENCE: not investigated further].

Follow-up: either amend the budget definition by ADR to "≤ one display frame + 1 ms at p95" (≈ 18 ms
at 60 Hz, 11 ms at 100 Hz), or add an on-demand present path to the GPUI macOS platform layer
(present the frame when the echo arrives instead of at the next tick); then rerun
`just bench --only B4` on a 60 Hz and a ≥ 100 Hz display.

### B3: passes, but at the limit

The remaining 0.46 % is not Heeler code: with the window visible, GPUI at the pinned revision keeps
a `CVDisplayLink` running and wakes the main thread at every display tick (~18–30 wakeups/s) to find
nothing to draw; `sample` shows all residual main-thread time under `gpui_macos::window::step` and
`gpui::window` request-frame handling, and the run-to-run spread (0.44–0.52 %) is that cost.
One earlier run measured 0.0 % with 0.03 wakeups/s [INFERENCE: the window was occluded, which
stops the link]. Follow-up: stop the link
while no frame is wanted (the `frame_waker` hook other platforms use) in the GPUI platform layer.
The sessions panel's relative-age refresh (every 30 s) is the only timer Heeler keeps while idle.

## Crash found while building the harness

Four `heeler-desktop` crash reports (SIGABRT, main thread) appeared while the harness was being
developed. Root cause: the first version of the bench key injector dispatched keystrokes from
inside a `Workbench` update (`WeakEntity::update_in`); the terminal view's key handling updates
the workbench again (focus, commands), and GPUI panics on re-entrant entity updates
(`cannot update … Workbench while it is already being updated`). The panic happened inside an
AppKit callback, which cannot unwind, so the process aborted and macOS showed its dialog.
Fixed by dispatching through the window context (`AsyncWindowContext::update`) instead; the
regression test (`scripted_keys_reach_the_focused_terminal_without_reentering_the_workbench`) fails
with exactly that panic message when the dispatch is moved back inside the update.
`heeler-desktop` now also installs a panic hook that appends the message, location and backtrace to
`$HEELER_HOME/logs/desktop.log` (`~/Library/Logs/Heeler/desktop.log` by default, next to
`heelerd.log`; one path function, `crash::log_dir`) before the process dies.
