//! Inputs the benchmarks hand to the desktop: a seeded `desktop-state.json` and key scripts.

use anyhow::{Result, bail};
use serde_json::{Value, json};

pub fn dock(panel: &str, size: f64, visible: bool) -> Value {
    json!({ "panels": [panel], "active": panel, "size": size, "visible": visible })
}

fn pane(id: usize, session: &str) -> Value {
    pane_of(id, vec![terminal_item(session)], 0)
}

/// A pane holding `items` (see [`terminal_item`], [`chat_item`], [`panel_item`]), `active` first.
pub fn pane_of(id: usize, items: Vec<Value>, active: usize) -> Value {
    json!({ "pane": { "id": id, "items": items, "active": active } })
}

pub fn terminal_item(session: &str) -> Value {
    json!({ "terminal": { "session": session } })
}

pub fn chat_item(session: &str) -> Value {
    json!({ "chat": { "session": session } })
}

pub fn panel_item(panel: &str) -> Value {
    json!({ "panel": { "panel": panel } })
}

pub fn split(axis: &str, children: Vec<Value>) -> Value {
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

/// A saved layout with the given docks and center (`panes` is the number of panes in it, so
/// that the next pane id does not clash); focus starts on pane 1.
pub fn custom_layout(
    left: Value,
    right: Value,
    bottom: Value,
    center: Value,
    panes: usize,
) -> Value {
    json!({
        "version": 1,
        "docks": { "left": left, "right": right, "bottom": bottom },
        "center": center,
        "zoomed": null,
        "focus": { "pane": 1 },
        "preset": "default",
        "previous_preset": null,
        "next_pane": panes + 1,
    })
}

/// A dock that is not shown.
pub fn hidden_dock(panel: &str, size: f64) -> Value {
    dock(panel, size, false)
}

/// A `HUTAN_BENCH_INPUT` script typing lowercase letters with an Enter after every 40 keys.
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
    fn custom_layouts_carry_their_items() {
        let center = split(
            "horizontal",
            vec![
                pane_of(1, vec![terminal_item("t"), chat_item("c")], 1),
                pane_of(2, vec![panel_item("tasks")], 0),
            ],
        );
        let layout = custom_layout(
            dock("sessions", 240.0, true),
            dock("agents", 300.0, true),
            hidden_dock("resources", 200.0),
            center,
            2,
        );
        let text = layout.to_string();
        assert!(text.contains(r#""chat":{"session":"c"}"#));
        assert!(text.contains(r#""panel":{"panel":"tasks"}"#));
        assert_eq!(layout["next_pane"], 3);
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
