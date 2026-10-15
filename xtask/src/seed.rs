//! Inputs the benchmarks hand to the desktop: a seeded `desktop-state.json` and key scripts.

use anyhow::{Result, bail};
use serde_json::{Value, json};

fn dock(panel: &str, size: f64, visible: bool) -> Value {
    json!({ "panels": [panel], "active": panel, "size": size, "visible": visible })
}

fn pane(id: usize, session: &str) -> Value {
    json!({ "pane": { "id": id, "items": [{ "terminal": { "session": session } }], "active": 0 } })
}

fn split(axis: &str, children: Vec<Value>) -> Value {
    let ratios = vec![1.0 / children.len() as f64; children.len()];
    json!({ "split": { "axis": axis, "ratios": ratios, "children": children } })
}

/// The `default` preset's docks around one terminal pane per session: one pane alone, two side
/// by side, four as a 2×2 grid. Focus starts on pane 1 (the first session).
///
/// Pane count is limited to 1, 2 or 4: the shapes the benchmarks need.
pub fn layout(sessions: &[String]) -> Result<Value> {
    let pane_at = |i: usize| pane(i + 1, &sessions[i]);
    let center = match sessions.len() {
        1 => pane_at(0),
        2 => split("horizontal", vec![pane_at(0), pane_at(1)]),
        4 => split(
            "horizontal",
            vec![
                split("vertical", vec![pane_at(0), pane_at(1)]),
                split("vertical", vec![pane_at(2), pane_at(3)]),
            ],
        ),
        n => bail!("layout() supports 1, 2 or 4 sessions, got {n}"),
    };
    Ok(json!({
        "version": 1,
        "docks": {
            "left": dock("sessions", 240.0, true),
            "right": dock("inspector", 300.0, true),
            "bottom": { "panels": [], "active": null, "size": 200.0, "visible": false },
        },
        "center": center,
        "zoomed": null,
        "focus": { "pane": 1 },
        "preset": "default",
        "previous_preset": null,
        "next_pane": sessions.len() + 1,
    }))
}

/// A `STAFFY_BENCH_INPUT` script typing lowercase letters with an Enter after every 40 keys.
pub fn typing_script(keys: usize, interval_ms: u64, start_delay_ms: u64, jitter_ms: u64) -> Value {
    let keys: Vec<String> = (1..=keys)
        .map(|n| {
            if n % 40 == 0 {
                "enter".to_owned()
            } else {
                char::from(b'a' + (n % 26) as u8).to_string()
            }
        })
        .collect();
    json!({ "interval_ms": interval_ms, "start_delay_ms": start_delay_ms, "jitter_ms": jitter_ms, "keys": keys })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typing_script_ends_lines() {
        let script = typing_script(80, 30, 500, 0);
        let keys = script["keys"].as_array().unwrap();
        assert_eq!(keys.len(), 80);
        assert_eq!(keys[39], "enter");
        assert_eq!(keys[0], "b");
    }

    #[test]
    fn layouts_hold_every_session() {
        for n in [1usize, 2, 4] {
            let sessions: Vec<String> = (0..n).map(|i| format!("s{i}")).collect();
            let text = layout(&sessions).unwrap().to_string();
            for s in &sessions {
                assert!(text.contains(s.as_str()));
            }
        }
        assert!(layout(&["a".to_owned(), "b".to_owned(), "c".to_owned()]).is_err());
    }
}
