//! Opt-in latency tracing for the performance budgets (`docs/perf.md`).
//!
//! `HEELER_TRACE_LATENCY=1` turns it on; events go to the file named by `HEELER_TRACE_FILE`
//! (appended, one JSON object per line) or to stderr without it. With the variable unset every
//! hook is one atomic load.
//!
//! Events:
//! * `{"ev":"startup","stage":…,"t_us":…}`: process milestones (`main`, `window_opened`,
//!   `first_frame`, `connected`, `first_terminal_painted`); `t_us` is wall-clock µs since the
//!   Unix epoch so a harness in another process can correlate with its own spawn time.
//! * `{"ev":"key","seq":…,"t_us":…,"key_to_sent_us":…,"sent_to_echo_us":…,"echo_to_paint_us":…,
//!   "total_us":…}`: one keystroke, from the moment the key event was handled to the end of the
//!   first paint of a frame that replaced the cursor row after the bytes were sent.
//! * `{"ev":"bench_start"}` / `{"ev":"bench_done"}`: the scripted key injection began / ended.

use std::collections::{HashSet, VecDeque};
use std::fs::OpenOptions;
use std::io::Write as _;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Tracer {
    sink: Mutex<Box<dyn std::io::Write + Send>>,
    startup_seen: Mutex<HashSet<&'static str>>,
}

static TRACER: LazyLock<Option<Tracer>> = LazyLock::new(init_tracer);
static KEY_SEQ: AtomicU64 = AtomicU64::new(0);

fn init_tracer() -> Option<Tracer> {
    let on = std::env::var("HEELER_TRACE_LATENCY").is_ok_and(|v| !v.is_empty() && v != "0");
    if !on {
        return None;
    }
    let sink: Box<dyn std::io::Write + Send> = match std::env::var_os("HEELER_TRACE_FILE")
        .filter(|p| !p.is_empty())
        .map(|path| OpenOptions::new().create(true).append(true).open(path))
    {
        Some(Ok(file)) => Box::new(file),
        Some(Err(err)) => {
            eprintln!("heeler: cannot open HEELER_TRACE_FILE: {err}; tracing to stderr");
            Box::new(std::io::stderr())
        }
        None => Box::new(std::io::stderr()),
    };
    Some(Tracer {
        sink: Mutex::new(sink),
        startup_seen: Mutex::new(HashSet::new()),
    })
}

fn tracer() -> Option<&'static Tracer> {
    TRACER.as_ref()
}

/// Whether tracing is on. Cheap enough for per-event call sites.
pub fn enabled() -> bool {
    tracer().is_some()
}

/// Wall-clock microseconds since the Unix epoch.
pub fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_micros()).unwrap_or(u64::MAX))
}

fn write_line(tracer: &Tracer, line: &str) {
    if let Ok(mut sink) = tracer.sink.lock() {
        // One write per line keeps lines whole even with several writers on the file.
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        let _ = sink.write_all(buf.as_bytes());
        let _ = sink.flush();
    }
}

/// Emits `{"ev":<ev>,"t_us":…}` (no-op when tracing is off).
pub fn event(ev: &str) {
    event_with(ev, "");
}

/// Like [`event`] with extra JSON members appended, e.g. `"window_active":true`.
pub fn event_with(ev: &str, members: &str) {
    if let Some(tracer) = tracer() {
        let extra = if members.is_empty() {
            String::new()
        } else {
            format!(",{members}")
        };
        write_line(
            tracer,
            &format!(r#"{{"ev":"{ev}","t_us":{}{extra}}}"#, now_us()),
        );
    }
}

/// Emits a startup milestone. A stage is reported at most once per process.
pub fn startup(stage: &'static str) {
    let Some(tracer) = tracer() else { return };
    let first = tracer
        .startup_seen
        .lock()
        .is_ok_and(|mut seen| seen.insert(stage));
    if first {
        write_line(
            tracer,
            &format!(
                r#"{{"ev":"startup","stage":"{stage}","t_us":{}}}"#,
                now_us()
            ),
        );
    }
}

/// Whether [`startup`] already reported `stage` (always false when tracing is off).
pub fn startup_seen(stage: &'static str) -> bool {
    tracer().is_some_and(|t| t.startup_seen.lock().is_ok_and(|seen| seen.contains(stage)))
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

/// One finished keystroke measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyLatency {
    pub seq: u64,
    pub key_to_sent: Duration,
    pub sent_to_echo: Duration,
    pub echo_to_paint: Duration,
    /// Part of `echo_to_paint`: until the frame that painted it started (waiting for a tick).
    pub echo_to_prepaint: Duration,
}

impl KeyLatency {
    pub fn total(&self) -> Duration {
        self.key_to_sent + self.sent_to_echo + self.echo_to_paint
    }

    fn emit(&self) {
        if let Some(tracer) = tracer() {
            write_line(
                tracer,
                &format!(
                    r#"{{"ev":"key","seq":{},"t_us":{},"key_to_sent_us":{},"sent_to_echo_us":{},"echo_to_paint_us":{},"echo_to_prepaint_us":{},"total_us":{}}}"#,
                    self.seq,
                    now_us(),
                    micros(self.key_to_sent),
                    micros(self.sent_to_echo),
                    micros(self.echo_to_paint),
                    micros(self.echo_to_prepaint),
                    micros(self.total()),
                ),
            );
        }
    }
}

#[derive(Debug)]
struct Pending {
    seq: u64,
    key_at: Instant,
    sent_at: Instant,
    echoed_at: Option<Instant>,
}

/// Correlates keystrokes with the frames that echo them: a key is *sent* once its bytes went to
/// the backend, *echoed* when a frame whose dirty rows include the cursor row is applied after
/// that, and *painted* when the next paint of the view finishes.
#[derive(Debug, Default)]
pub struct LatencyTracker {
    pending: VecDeque<Pending>,
    /// When the key event being handled started, until its bytes are sent.
    key_at: Option<Instant>,
    /// When the current frame's prepaint started.
    prepaint_at: Option<Instant>,
    paint_window_start: Option<Instant>,
    paints: u32,
}

/// Keys waiting for an echo beyond this are dropped (a program that never echoes).
const MAX_PENDING: usize = 2048;

impl LatencyTracker {
    /// A key event is about to be handled.
    pub fn key_started(&mut self, at: Instant) {
        self.key_at = Some(at);
    }

    /// The key event ended without sending anything.
    pub fn key_unsent(&mut self) {
        self.key_at = None;
    }

    /// The bytes of the key being handled were handed to the backend.
    pub fn key_sent(&mut self, at: Instant) {
        let Some(key_at) = self.key_at.take() else {
            return;
        };
        if self.pending.len() >= MAX_PENDING {
            self.pending.pop_front();
        }
        self.pending.push_back(Pending {
            seq: KEY_SEQ.fetch_add(1, Ordering::Relaxed),
            key_at,
            sent_at: at,
            echoed_at: None,
        });
    }

    /// A frame was applied; `replaced_cursor_row` tells whether it changed the cursor's row.
    pub fn frame_applied(&mut self, at: Instant, replaced_cursor_row: bool) {
        if !replaced_cursor_row {
            return;
        }
        for pending in &mut self.pending {
            pending.echoed_at.get_or_insert(at);
        }
    }

    /// The view finished painting: every echoed key is complete.
    pub fn painted(&mut self, at: Instant) -> Vec<KeyLatency> {
        let mut done = Vec::new();
        let prepaint_at = self.prepaint_at.take();
        self.pending.retain(|p| match p.echoed_at {
            Some(echoed_at) => {
                done.push(KeyLatency {
                    seq: p.seq,
                    key_to_sent: p.sent_at.saturating_duration_since(p.key_at),
                    sent_to_echo: echoed_at.saturating_duration_since(p.sent_at),
                    echo_to_paint: at.saturating_duration_since(echoed_at),
                    echo_to_prepaint: prepaint_at
                        .map_or(Duration::ZERO, |t| t.saturating_duration_since(echoed_at)),
                });
                false
            }
            None => true,
        });
        done
    }

    /// The view started preparing the frame that is about to be painted.
    pub fn prepaint(&mut self, at: Instant) {
        self.prepaint_at = Some(at);
    }

    /// Emits every completed measurement as a `key` event, and once per second how often this
    /// view painted (`{"ev":"paints","n":…,"span_us":…}`): while a terminal is flooded that is
    /// the display's frame rate.
    pub fn painted_and_emit(&mut self, at: Instant) {
        for latency in self.painted(at) {
            latency.emit();
        }
        let start = *self.paint_window_start.get_or_insert(at);
        self.paints += 1;
        let span = at.saturating_duration_since(start);
        if span >= Duration::from_secs(1) {
            if let Some(tracer) = tracer() {
                write_line(
                    tracer,
                    &format!(
                        r#"{{"ev":"paints","t_us":{},"n":{},"span_us":{}}}"#,
                        now_us(),
                        self.paints,
                        micros(span)
                    ),
                );
            }
            self.paint_window_start = Some(at);
            self.paints = 0;
        }
    }

    pub fn is_idle(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(base: Instant, n: u64) -> Instant {
        base + Duration::from_millis(n)
    }

    #[test]
    fn key_is_complete_only_after_an_echoing_frame_and_a_paint() {
        let t0 = Instant::now();
        let mut tracker = LatencyTracker::default();
        tracker.key_started(t0);
        tracker.key_sent(ms(t0, 1));
        // A frame that does not touch the cursor row echoes nothing.
        tracker.frame_applied(ms(t0, 4), false);
        assert!(tracker.painted(ms(t0, 5)).is_empty());
        tracker.frame_applied(ms(t0, 7), true);
        // The echo is not complete until painted.
        let done = tracker.painted(ms(t0, 10));
        assert_eq!(done.len(), 1);
        assert_eq!(done[0].key_to_sent, Duration::from_millis(1));
        assert_eq!(done[0].sent_to_echo, Duration::from_millis(6));
        assert_eq!(done[0].echo_to_paint, Duration::from_millis(3));
        assert_eq!(done[0].total(), Duration::from_millis(10));
        assert!(tracker.is_idle());
    }

    #[test]
    fn one_frame_completes_every_key_sent_before_it() {
        let t0 = Instant::now();
        let mut tracker = LatencyTracker::default();
        for n in 0..3 {
            tracker.key_started(ms(t0, n * 2));
            tracker.key_sent(ms(t0, n * 2 + 1));
        }
        tracker.frame_applied(ms(t0, 8), true);
        // A key typed after the echoing frame waits for the next one.
        tracker.key_started(ms(t0, 9));
        tracker.key_sent(ms(t0, 9));
        let done = tracker.painted(ms(t0, 12));
        assert_eq!(done.len(), 3);
        assert!(!tracker.is_idle());
        tracker.frame_applied(ms(t0, 14), true);
        assert_eq!(tracker.painted(ms(t0, 16)).len(), 1);
    }

    #[test]
    fn unsent_keys_are_not_tracked() {
        let t0 = Instant::now();
        let mut tracker = LatencyTracker::default();
        tracker.key_started(t0);
        tracker.key_unsent();
        tracker.key_sent(ms(t0, 1));
        tracker.frame_applied(ms(t0, 2), true);
        assert!(tracker.painted(ms(t0, 3)).is_empty());
    }
}
