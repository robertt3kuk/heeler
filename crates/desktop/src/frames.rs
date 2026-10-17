//! Frame timing for the performance benchmarks (`docs/perf.md`, B7–B10).
//!
//! GPUI draws immediate-mode: every frame renders the `Workbench` and everything it contains.
//! A frame therefore starts when [`begin`] runs at the top of `Workbench::render` and ends when
//! the [`probe`] element, the last child of the workbench root, is painted. The span between the
//! two is what Hutan spends on a frame (render, layout, prepaint, paint of the scene; the GPU
//! submission and the wait for the display are not in it).
//!
//! Nothing here costs anything unless `HUTAN_TRACE_LATENCY` is on: [`begin`] is one atomic load
//! and [`probe`] is not added to the tree. While the bench driver records a phase
//! ([`set_recording`]), every frame becomes a `{"ev":"frame",…}` trace line, and every operation
//! the driver started ([`Op`]) becomes an `{"ev":"op",…}` line when the first frame that shows
//! its result has been painted:
//!
//! ```json
//! {"ev":"frame","t_us":…,"work_us":…,"interval_us":…}
//! {"ev":"op","kind":"key","t_us":…,"handle_us":…,"wait_us":…,"work_us":…,"total_us":…}
//! ```
//!
//! `total` is dispatch → end of the frame; `handle` is the time the input handlers took,
//! `wait` the time until the frame began (the display tick), `work` the frame's own time. The
//! Hutan path of an operation, as B4a measures it for keys, is `handle + work`.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{IntoElement, Styled, canvas, px};
use hutan_terminal_view::trace;

static RECORDING: AtomicBool = AtomicBool::new(false);

/// An operation the bench driver started and is waiting to see painted.
#[derive(Debug, Clone, Copy)]
struct Pending {
    kind: &'static str,
    started: Instant,
    /// How long the synchronous input handlers took (the dispatch returned after this).
    handled: Duration,
    wall_us: u64,
}

/// What one frame saw, for the operations waiting on it.
#[derive(Debug, Clone, Copy, Default)]
struct Seen {
    /// Content of an opened item (a chat with entries, a terminal with text) was painted.
    content: bool,
}

#[derive(Debug, Default)]
struct Recorder {
    begun: Option<Instant>,
    last_end: Option<Instant>,
    seen: Seen,
    text_paints: u64,
    pending: Vec<Pending>,
    frames: u64,
}

/// A finished frame, as the recorder reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameRecord {
    pub work: Duration,
    /// Since the previous frame ended; absent for the first.
    pub interval: Option<Duration>,
}

/// A finished operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpRecord {
    pub kind: &'static str,
    pub wall_us: u64,
    pub handle: Duration,
    pub wait: Duration,
    pub work: Duration,
    pub total: Duration,
}

impl Recorder {
    fn begin(&mut self, now: Instant, text_paints: u64) {
        self.begun = Some(now);
        self.seen = Seen::default();
        self.text_paints = text_paints;
    }

    fn end(
        &mut self,
        now: Instant,
        text_paints: u64,
        wait_for_content: bool,
    ) -> (FrameRecord, Vec<OpRecord>) {
        self.seen.content |= text_paints != self.text_paints;
        let begun = self.begun.take().unwrap_or(now);
        let record = FrameRecord {
            work: now.saturating_duration_since(begun),
            interval: self
                .last_end
                .map(|last| now.saturating_duration_since(last)),
        };
        self.last_end = Some(now);
        self.frames += 1;
        let mut done = Vec::new();
        if !wait_for_content || self.seen.content {
            for op in self.pending.drain(..) {
                // An operation whose dispatch is after the frame began did not make this frame.
                if op.started + op.handled > begun {
                    // Kept for the next frame (cannot happen on one thread; kept for safety).
                    continue;
                }
                done.push(OpRecord {
                    kind: op.kind,
                    wall_us: op.wall_us,
                    handle: op.handled,
                    wait: begun.saturating_duration_since(op.started + op.handled),
                    work: record.work,
                    total: now.saturating_duration_since(op.started),
                });
            }
        }
        (record, done)
    }
}

thread_local! {
    // GPUI draws on the main thread only.
    static RECORDER: RefCell<Recorder> = RefCell::new(Recorder::default());
}

/// Whether frame timing is available (tracing is on).
pub fn enabled() -> bool {
    trace::enabled()
}

/// Starts or stops writing `frame` and `op` lines.
pub fn set_recording(on: bool) {
    RECORDING.store(on, Ordering::Relaxed);
}

fn recording() -> bool {
    RECORDING.load(Ordering::Relaxed)
}

/// Marks the start of a frame: call at the top of the root view's render.
pub fn begin() {
    if !enabled() {
        return;
    }
    RECORDER.with(|r| r.borrow_mut().begin(Instant::now(), trace::text_paints()));
}

/// Says that content of an opened item (a chat with entries, a terminal with text) was drawn in
/// this frame; operations that open something wait for it.
pub fn content_painted() {
    if !enabled() {
        return;
    }
    RECORDER.with(|r| r.borrow_mut().seen.content = true);
}

/// How many frames have been painted since the process started.
pub fn frame_count() -> u64 {
    RECORDER.with(|r| r.borrow().frames)
}

/// Operations dispatched and not yet seen painted.
pub fn pending_ops() -> usize {
    RECORDER.with(|r| r.borrow().pending.len())
}

/// The element that ends a frame: add it as the last child of the root.
pub fn probe() -> impl IntoElement {
    canvas(|_, _, _| (), |_, (), _, _| end())
        .absolute()
        .size(px(0.0))
}

fn end() {
    let now = Instant::now();
    let waiting = RECORDER.with(|r| r.borrow().pending.iter().any(|op| op.kind != "key"));
    let (frame, ops) = RECORDER.with(|r| {
        // Typing and switching show their result in the first frame; an `open` waits for the
        // content of what was opened.
        r.borrow_mut().end(now, trace::text_paints(), waiting)
    });
    if !recording() {
        return;
    }
    let micros = |d: Duration| u64::try_from(d.as_micros()).unwrap_or(u64::MAX);
    trace::event_with(
        "frame",
        &format!(
            r#""work_us":{},"interval_us":{}"#,
            micros(frame.work),
            frame.interval.map_or(0, micros)
        ),
    );
    for op in ops {
        trace::event_with(
            "op",
            &format!(
                r#""kind":"{}","start_us":{},"handle_us":{},"wait_us":{},"work_us":{},"total_us":{}"#,
                op.kind,
                op.wall_us,
                micros(op.handle),
                micros(op.wait),
                micros(op.work),
                micros(op.total)
            ),
        );
    }
}

/// An operation in flight: created before the input is dispatched, finished right after.
#[derive(Debug)]
pub struct Op {
    kind: &'static str,
    started: Instant,
    wall_us: u64,
}

impl Op {
    /// Call just before dispatching the input.
    pub fn start(kind: &'static str) -> Self {
        Self {
            kind,
            started: Instant::now(),
            wall_us: trace::now_us(),
        }
    }

    /// Call right after the dispatch returned: the operation now waits for the next frame.
    pub fn dispatched(self) {
        if !enabled() {
            return;
        }
        let pending = Pending {
            kind: self.kind,
            started: self.started,
            handled: self.started.elapsed(),
            wall_us: self.wall_us,
        };
        RECORDER.with(|r| r.borrow_mut().pending.push(pending));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_frame_is_the_span_from_begin_to_end_and_intervals_chain() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.begin(t0, 0);
        let (first, _) = r.end(t0 + ms(3), 0, false);
        assert_eq!(first.work, ms(3));
        assert_eq!(first.interval, None);
        r.begin(t0 + ms(16), 0);
        let (second, _) = r.end(t0 + ms(21), 0, false);
        assert_eq!(second.work, ms(5));
        assert_eq!(second.interval, Some(ms(18)));
        assert_eq!(r.frames, 2);
    }

    #[test]
    fn an_operation_ends_with_the_first_frame_after_its_dispatch() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.pending.push(Pending {
            kind: "key",
            started: t0,
            handled: ms(1),
            wall_us: 7,
        });
        r.begin(t0 + ms(9), 0);
        let (_, ops) = r.end(t0 + ms(12), 0, false);
        assert_eq!(ops.len(), 1);
        let op = ops[0];
        assert_eq!(op.handle, ms(1));
        assert_eq!(op.wait, ms(8));
        assert_eq!(op.work, ms(3));
        assert_eq!(op.total, ms(12));
        assert!(r.pending.is_empty());
    }

    #[test]
    fn an_open_waits_for_content_to_be_painted() {
        let t0 = Instant::now();
        let mut r = Recorder::default();
        r.pending.push(Pending {
            kind: "open",
            started: t0,
            handled: ms(1),
            wall_us: 0,
        });
        r.begin(t0 + ms(5), 0);
        let (_, none) = r.end(t0 + ms(6), 0, true);
        assert!(none.is_empty(), "no content yet");
        r.begin(t0 + ms(20), 0);
        r.seen.content = true;
        let (_, done) = r.end(t0 + ms(30), 0, true);
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].total, ms(30));
    }
}
