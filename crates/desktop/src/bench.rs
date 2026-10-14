//! Scripted key injection for the latency benchmarks (`cargo run -p xtask -- bench`).
//!
//! `HEELER_BENCH_INPUT=<script.json>` makes the window type a fixed script into the focused
//! pane's terminal:
//!
//! ```json
//! { "interval_ms": 30, "start_delay_ms": 1000, "keys": ["a", "b", "enter"] }
//! ```
//!
//! Each key is a GPUI keystroke (`a`, `space`, `enter`, `ctrl-c`, …) handed to
//! [`gpui::Window::dispatch_keystroke`], the path real key events take: dispatch tree,
//! the terminal view's key handler, key encoding, `TerminalHandle::input`. Keys are scheduled
//! at `start + n * interval` (no drift); the schedule starts `start_delay_ms` after the first
//! terminal painted text. With `HEELER_TRACE_LATENCY=1` the run is framed by `bench_start` /
//! `bench_done` trace events, both carrying the refresh interval of the window's display
//! (`refresh_interval_us`). Without the variable this module does nothing.

use std::time::{Duration, Instant};

use gpui::{Context, Keystroke, Window};
use heeler_terminal_view::trace;
use serde::Deserialize;

use crate::workbench::Workbench;

const POLL: Duration = Duration::from_millis(20);

/// `"refresh_interval_us":…` for a trace event: the refresh interval of the display the window
/// is on, as the platform reports it (the maximum rate on variable refresh rate displays;
/// `null` when unknown). B4b is judged against it (ADR 0011).
fn display_members(cx: &mut gpui::AsyncWindowContext) -> String {
    let interval = cx
        .update(|window, cx| window.refresh_interval(cx))
        .ok()
        .flatten();
    match interval {
        Some(interval) => format!(r#""refresh_interval_us":{}"#, interval.as_micros()),
        None => r#""refresh_interval_us":null"#.to_owned(),
    }
}

/// The key script.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Script {
    pub interval_ms: u64,
    #[serde(default)]
    pub start_delay_ms: u64,
    pub keys: Vec<String>,
    /// Each key is delayed by a deterministic pseudo-random 0..`jitter_ms` on top of the fixed
    /// schedule. A strictly periodic injector can lock to the display's vsync period and measure
    /// one phase of it for the whole run; typing by hand has no such phase.
    #[serde(default)]
    pub jitter_ms: u64,
}

impl Script {
    /// Parses the JSON script and checks every key.
    pub fn parse(json: &str) -> Result<Self, String> {
        let script: Script = serde_json::from_str(json).map_err(|e| e.to_string())?;
        if script.interval_ms == 0 {
            return Err("interval_ms must be positive".to_owned());
        }
        for key in &script.keys {
            if key.is_empty() {
                return Err("empty key".to_owned());
            }
            Keystroke::parse(key).map_err(|e| format!("key {key:?}: {e}"))?;
        }
        Ok(script)
    }

    /// Delay of key `n` after its scheduled time (a Weyl sequence: evenly spread, repeatable).
    pub fn jitter(&self, n: usize) -> Duration {
        if self.jitter_ms == 0 {
            return Duration::ZERO;
        }
        let micros = self.jitter_ms * 1000;
        let phase = (n as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 11;
        Duration::from_micros(phase % micros)
    }
}

/// Starts the script named by `HEELER_BENCH_INPUT`, if any.
pub fn start(window: &mut Window, cx: &mut Context<Workbench>) {
    let Some(path) = std::env::var_os("HEELER_BENCH_INPUT").filter(|p| !p.is_empty()) else {
        return;
    };
    let script = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|json| Script::parse(&json))
    {
        Ok(script) => script,
        Err(err) => {
            tracing::error!("HEELER_BENCH_INPUT {}: {err}", path.to_string_lossy());
            return;
        }
    };
    run(script, window, cx);
}

/// Types `script` into the focused terminal of the workbench.
///
/// The keys must not be dispatched from inside a `Workbench` update: the key handlers update
/// the workbench themselves, and re-entering a leased entity panics (and in a window callback
/// that aborts the process). The task therefore only touches the workbench for the focus move
/// and dispatches through the window context.
pub fn run(script: Script, window: &mut Window, cx: &mut Context<Workbench>) {
    cx.spawn_in(window, async move |this, cx| {
        // Wait for something to type into: the first terminal that painted text.
        if trace::enabled() {
            while !trace::startup_seen("first_terminal_painted") {
                cx.background_executor().timer(POLL).await;
            }
        }
        cx.background_executor()
            .timer(Duration::from_millis(script.start_delay_ms))
            .await;
        if this
            .update_in(cx, |workbench, window, cx| workbench.focus_root(window, cx))
            .is_err()
        {
            return;
        }
        let active = cx
            .update(|window, _| window.is_window_active())
            .unwrap_or(false);
        let display = display_members(cx);
        trace::event_with(
            "bench_start",
            &format!(r#""window_active":{active},{display}"#),
        );
        let interval = Duration::from_millis(script.interval_ms);
        let started = Instant::now();
        for (n, key) in script.keys.iter().enumerate() {
            let due = started + interval * u32::try_from(n).unwrap_or(u32::MAX) + script.jitter(n);
            cx.background_executor()
                .timer(due.saturating_duration_since(Instant::now()))
                .await;
            let Ok(keystroke) = Keystroke::parse(key) else {
                continue;
            };
            // Not inside a workbench update: the key handlers update the workbench themselves.
            if cx
                .update(|window, cx| {
                    window.dispatch_keystroke(keystroke, cx);
                })
                .is_err()
            {
                return;
            }
        }
        let display = display_members(cx);
        trace::event_with("bench_done", &display);
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_script() {
        let script =
            Script::parse(r#"{"interval_ms":30,"keys":["a","space","enter","ctrl-c"]}"#).unwrap();
        assert_eq!(script.interval_ms, 30);
        assert_eq!(script.start_delay_ms, 0);
        assert_eq!(script.keys.len(), 4);
    }

    #[test]
    fn rejects_bad_scripts() {
        assert!(Script::parse("nope").is_err());
        assert!(Script::parse(r#"{"interval_ms":0,"keys":[]}"#).is_err());
        assert!(Script::parse(r#"{"interval_ms":5,"keys":[""]}"#).is_err());
    }
}
