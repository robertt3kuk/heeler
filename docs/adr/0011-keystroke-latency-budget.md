# 0011. Keystroke latency budget measured against the display frame

- Status: accepted
- Date: 2026-10-14
- Milestone: M2

## Context
`vision.md` budget B4 said "keystroke → glyph on screen ≤ 16 ms p95". The M2-F bench (`docs/perf.md`) measured 18.0 ms p95 (median of 3; orchestrator rerun 18.5 ms) on an Apple M4 with a 60 Hz display. Decomposition from `STAFFY_TRACE_LATENCY`: Staffy's own work on the path (key handling, encoding, IPC, PTY echo, engine, frame, paint) is ≈ 1.5 ms; the rest is waiting for the next display-link tick (16.7 ms at 60 Hz) at a random phase. GPUI at the pinned revision draws only inside display-link callbacks, like other vsync-bound terminals (Ghostty, Zed). A fixed 16 ms p95 is unreachable at 60 Hz regardless of our code, and it would be trivially met at 120 Hz, so it does not measure what we control.

## Options
| Option | Pros | Cons |
|---|---|---|
| Keep 16 ms, patch GPUI to present on demand outside the display link | Lower worst case | Fork/patch of GPUI's macOS layer; tearing/pacing risks; maintenance on every GPUI bump |
| **Measure our share: next display frame + ≤ 2 ms** | Measures what Staffy controls; refresh-rate independent; regression-sensitive | Absolute latency still depends on the display |

## Decision
B4 becomes two numbers, both p95 over 500 keystrokes into a `cat` session:
- **B4a — Staffy path ≤ 2 ms**: from key event handled to the frame containing the echoed glyph being submitted for presentation (excludes display-link wait).
- **B4b — on-screen ≤ one display frame + 2 ms** (16.7 + 2 = 18.7 ms at 60 Hz, 8.3 + 2 = 10.3 ms at 120 Hz), using the display's refresh rate reported at bench time.

`xtask bench` reports both and fails on either. `vision.md` § Budgets is updated accordingly.

## Consequences
- The current build passes B4b (18.0–18.5 ms at 60 Hz) and B4a must be reported by the bench (add the split if the tracer only reports end-to-end).
- On-demand presentation stays a possible future optimization, tracked in the roadmap backlog, not a budget requirement.

## Evidence
`docs/perf.md` § B4.
