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

Report `target/bench/1792001491.json` (commit `8a1de1d`, ADR 0013 store; before = the M2-F table, same harness).

| # | Metric | Budget | Median of 3 | Runs | Before | Result |
|---|---|---|---|---|---|---|
| B1 | Idle desktop RSS, 1 window, 4 terminals | ≤ 150 MB | **110.9 MB** | 110.5, 110.9, 111.8 | 102.7 MB | pass |
| B2 | Idle daemon RSS, 10 sessions × 10k-line scrollback, 80 columns, desktop attached | ≤ 120 MB | **40.1 MB** | 40.1, 39.8, 40.1 | 254.6 MB | pass |
| B2w | Same at 200 columns (~190-column lines), no client | ≤ 150 MB | **84.6 MB** | 84.6, 84.6, 84.9 | 538 MB (measured with the old store, B2w did not exist) | pass |
| B3 | Idle CPU, daemon + desktop, 30 s | ≤ 0.5 % | **0.48 %** | 0.48, 0.43, 0.58 | 0.46 % | pass, at the limit |
| B4a | Heeler path: key handled → frame with the echo submitted, p95 over 500 keys | ≤ 2 ms | **1.34 ms** | 1.17, 1.35, 1.34 | n/a (new) | pass |
| B4b | On screen, p95 over 500 keys | ≤ 1 frame + 2 ms = 18.7 ms (60 Hz, read from the platform) | **18.50 ms** | 106.9*, 18.5, 18.0 | 18.0 ms (vs 16 ms) | pass, marginal (below) |
| B5 | 200 MB+ flood: p95 key latency in a second terminal | ≤ 50 ms | **17.25 ms** | 17.25, 17.27, 17.21 | 16.9 ms | pass |
| B5 | 200 MB+ flood: daemon RSS growth | ≤ 50 MB | **2.28 MB** | 2.30, 2.25, 2.28 | 13.6 MB | pass |
| B6 | Cold start → first terminal paint with its prompt | ≤ 500 ms | **196 ms** | 179, 199, 196 | 213 ms | pass |

\* the window was not being presented at the display rate during that run (see B4b below); the median is not affected, and the harness now repeats such runs (and fails instead of reporting them).

A second complete run (`1792002157.json`) agrees on everything except B4b: B2 40.0 MB, B2w 84.9 MB, B4a 1.33 ms, B5 17.3 ms / 2.3 MB, B6 181 ms, B1 108.5 MB; B4b gave 18.90 ms and 18.59 ms in its two valid runs, i.e. a median **above** 18.7 ms (FAIL by 0.2 ms; the third run was spoiled and repeated 3 times, see below). B3 read 0.00 % there because the window was occluded. **B4b therefore passes or misses by a few tenths of a millisecond depending on the run**; it was not possible to produce a clean third full report (the session locked, `gpui_macos: could not create display link, code: -6661`).

Other numbers behind the table (medians): B1 physical footprint 68.8 MB; daemon with the 10
sessions before the desktop attached 208 MB; B3 desktop 0.46 %, daemon 0.00 %; B4 median latency
10.1 ms; B5 median 9.0 ms, 245 keystrokes measured while the flood ran (12.2 s); B6 timeline
(ms after spawn): `main` 15, window opened 142, first frame 159, connected 171, first terminal text
painted 213.

`xtask bench` exits non-zero while a budget is missed. The gate (`just check`) does not run it.

## Method

`cargo run -p xtask --release -- bench [--only B1,B4] [--runs N] [--json] [--sample]
[--key-interval-ms N]` builds `heelerd`, `heeler-desktop` and `heeler` in release, then for every
scenario and run: creates a temporary `HEELER_HOME`, starts `heelerd`, seeds sessions and a
`desktop-state.json`, launches the desktop (the window appears, no screenshots), measures, and
tears down. The report goes to `target/bench/<unix-time>.json` (all runs, every stage metric).

- **B2w.** Same load as B2 (10 sessions × 12 000 coloured lines, 10 000-line scrollback) in 200×50
  terminals with ~190-column lines, no client attached; daemon RSS after the sessions settle.
- **B1/B2/B3 (one scenario).** 10 sessions each print 12 000 coloured 80-column lines (the
  10 000-line scrollback overflows) and idle in `cat`. Daemon RSS is sampled; the desktop opens a
  seeded 2×2 layout of 4 of them; after 10 s idle the desktop and daemon are sampled
  (`proc_pid_rusage`: resident size, physical footprint, user+system CPU time, idle wakeups), and
  again after 30 s. B3 is the CPU time delta of both processes over those 30 s.
- **B4a / B4b.** One `cat`-like session (`printf 'bench> '; exec cat`) in the only pane. The desktop types
  500 keys at one key per 100 ms plus a deterministic 0–17 ms jitter (`HEELER_BENCH_INPUT`, below).
  Latency is the in-app trace: key event handled → first paint that follows the daemon frame
  replacing the cursor row or moving the cursor. **B4b** is that whole latency. **B4a** is the same
  minus the wait for the display tick (`echo_to_prepaint`): `key_to_sent + sent_to_echo +
  (echo_to_paint − echo_to_prepaint)`, i.e. key handled → the frame with the echo is painted
  (submitted to the renderer). B4b's limit is one display frame + 2 ms (ADR 0011): the desktop
  reports `Window::refresh_interval` (GPUI's cached `maximumFramesPerSecond` of the screen the
  window is on) in the `bench_start`/`bench_done` events, the report records it
  (`b4_refresh_interval_ms`, `b4_refresh_hz`; 60 Hz = 16.67 ms on this run) and the run fails when it
  is missing or changes during the run. A run whose window was inactive or not presented at the
  display rate (median wait for the display tick > 3 frames: a locked, sleeping or occluded
  display) is repeated up to 4 times and then fails the bench; `caffeinate -d -i` keeps the display
  awake while the bench runs. The jitter is needed: a strictly periodic injector
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

### B2: fixed by the compact scrollback store (ADR 0013)

Root cause (unchanged, measured earlier): `alacritty_terminal` stores every cell in 24 bytes, so a
10 000-line history is 19.2 MB per session at 80 columns and 48 MB at 200. Fix: the engine keeps
the viewport plus 256 history rows; older rows are spilled after every 2 KiB slice of output into a
per-terminal packed store (text bytes, row index, style spans keyed by byte offset). Result: B2
254.6 → 40.1 MB, B2w 84.6 MB (it was 538 MB with the old store). `b2_daemon_rss_detached_mb`
(before the desktop attached) 37.8 MB; attaching no longer reflows the history, so the desktop adds
only ~2 MB.

What was tried and rejected: spilling only after output pauses (cheaper during floods, but the
engine then holds its whole history during every burst and the process keeps that memory resident:
B2 218.9 MB, B2w 538 MB). macOS does not return freed small allocations to the resident set
(`malloc_zone_pressure_relief` released nothing in a direct test), so the peak, not the idle
live size, decides. Throughput of the mixed 200 MB `flood` example: 138 MB/s with the store vs
154 MB/s before (release; peak RSS 14.9 vs 26.2 MiB).

### B4: split by ADR 0011

Where the time goes (medians over the three runs): key event → bytes sent 0.02 ms; sent → echo
applied 0.3–0.5 ms; applied → start of the paint 9–10 ms (waiting for the display tick); paint
≈ 0.5 ms. B4a (Heeler path) is **1.3 ms p95**, well inside 2 ms. B4b is the display-tick wait on
top: p95 ≈ one frame (16.7 ms) + ~1.3–2.2 ms, against a limit of one frame + 2 ms, so it sits on
the limit: 18.0, 18.5, 18.6, 18.9 ms in the valid runs of two reports. Reducing it further needs
the on-demand present path (roadmap backlog), not Heeler-side work.

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
