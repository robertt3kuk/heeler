//! `xtask fake-codex`: a stand-in for `codex app-server` that the chat benchmarks (B7, B8) point
//! the daemon's `[codex]` config at. No model, no network, no login: it speaks just enough of
//! the app-server protocol (the 0.162 shapes of `crates/agents/tests/fixtures/codex-0.162`) and
//! answers a user message by playing a script chosen by the message's text:
//!
//! - `bench:fill <units>`: a conversation of `units` exchanges as fast as the pipe takes it, each
//!   a user message, an assistant message with markdown and a code block, a shell command with
//!   output and a patch: 9 journal events per unit.
//! - `bench:stream <deltas_per_second> <seconds>`: one assistant message streamed as paced
//!   deltas (markdown with code blocks), then completed.
//! - anything else: a short reply.
//!
//! Everything it says is synthetic; it never touches the user's Codex.

use std::io::{self, BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{Value, json};

const THREAD: &str = "01a00000-0000-7000-8000-000000000001";

type Out = Arc<Mutex<io::Stdout>>;

fn send(out: &Out, message: &Value) {
    if let Ok(mut stdout) = out.lock() {
        let _ = writeln!(stdout, "{message}");
        let _ = stdout.flush();
    }
}

fn notify(out: &Out, method: &str, params: Value) {
    send(out, &json!({ "method": method, "params": params }));
}

fn reply(out: &Out, id: &Value, result: Value) {
    send(out, &json!({ "id": id, "result": result }));
}

pub fn run() -> Result<()> {
    let out: Out = Arc::new(Mutex::new(io::stdout()));
    let cancel = Arc::new(AtomicBool::new(false));
    let mut turns = 0u32;
    for line in io::stdin().lock().lines() {
        let Ok(message) = serde_json::from_str::<Value>(&line?) else {
            continue;
        };
        let Some(method) = message["method"].as_str() else {
            continue;
        };
        let id = message.get("id").cloned();
        let params = &message["params"];
        match (method, id) {
            ("initialize", Some(id)) => reply(
                &out,
                &id,
                json!({"userAgent": "hutan/0.162.1 (Mac OS; arm64)", "codexHome": "/tmp/codex-home",
                       "platformFamily": "unix", "platformOs": "macos"}),
            ),
            ("model/list", Some(id)) => reply(
                &out,
                &id,
                json!({"data": [{"id": "gpt-6.1-sol", "model": "gpt-6.1-sol", "displayName": "Bench",
                    "description": "synthetic", "hidden": false, "isDefault": true,
                    "supportedReasoningEfforts": [{"reasoningEffort": "low", "description": "low"}],
                    "defaultReasoningEffort": "low"}], "nextCursor": null}),
            ),
            ("thread/start" | "thread/resume", Some(id)) => reply(
                &out,
                &id,
                json!({"thread": {"id": THREAD}, "model": "gpt-6.1-sol"}),
            ),
            ("turn/start", Some(id)) => {
                turns += 1;
                let turn = format!("01a00000-0000-7000-8000-{turns:012}");
                reply(
                    &out,
                    &id,
                    json!({"turn": {"id": turn, "status": "inProgress"}}),
                );
                let text = params["input"][0]["text"].as_str().unwrap_or("").to_owned();
                cancel.store(false, Ordering::SeqCst);
                let (out, cancel) = (out.clone(), cancel.clone());
                std::thread::spawn(move || play(&out, &cancel, &turn, &text));
            }
            ("turn/interrupt", Some(id)) => {
                cancel.store(true, Ordering::SeqCst);
                reply(&out, &id, json!({}));
            }
            (_, Some(id)) => send(
                &out,
                &json!({"id": id, "error": {"code": -32601, "message": "method not found"}}),
            ),
            _ => {}
        }
    }
    Ok(())
}

fn turn_json(turn: &str, status: &str) -> Value {
    json!({"id": turn, "rootTurnId": turn, "items": [], "itemsView": "summary", "status": status,
           "error": null, "startedAt": 1, "completedAt": null, "durationMs": null})
}

fn item_event(out: &Out, method: &str, turn: &str, item: Value) {
    notify(
        out,
        method,
        json!({"item": item, "threadId": THREAD, "turnId": turn, "startedAtMs": 1, "completedAtMs": 1}),
    );
}

fn play(out: &Out, cancel: &AtomicBool, turn: &str, text: &str) {
    notify(
        out,
        "turn/started",
        json!({"threadId": THREAD, "turn": turn_json(turn, "inProgress")}),
    );
    let words: Vec<&str> = text.split_whitespace().collect();
    match words.as_slice() {
        ["bench:fill", units] => fill(out, turn, units.parse().unwrap_or(10)),
        ["bench:stream", rate, secs] => stream(
            out,
            cancel,
            turn,
            rate.parse().unwrap_or(200),
            secs.parse().unwrap_or(20),
        ),
        _ => message(
            out,
            turn,
            "msg_short",
            &format!("echo: {text}"),
            "final_answer",
        ),
    }
    let status = if cancel.load(Ordering::SeqCst) {
        "interrupted"
    } else {
        "completed"
    };
    notify(
        out,
        "turn/completed",
        json!({"threadId": THREAD, "turn": turn_json(turn, status)}),
    );
}

fn message(out: &Out, turn: &str, id: &str, text: &str, phase: &str) {
    let item = |text: &str| {
        json!({"type": "agentMessage", "id": id, "text": text, "phase": phase,
               "memoryCitation": null, "delivery": null, "questions": null})
    };
    item_event(out, "item/started", turn, item(""));
    notify(
        out,
        "item/agentMessage/delta",
        json!({"threadId": THREAD, "turnId": turn, "itemId": id, "delta": text}),
    );
    item_event(out, "item/completed", turn, item(text));
}

/// A paragraph of markdown with a heading, a list, inline code, a link and a code block.
pub fn markdown(n: usize) -> String {
    format!(
        "## Step {n}: tighten the retry path\n\n\
         The handler in `crates/daemon/src/chat.rs` retries **every** error, including the ones that \
         cannot succeed. Three changes make it behave:\n\n\
         - classify the error before the first retry (see [the notes](https://example.com/notes/{n}))\n\
         - back off with *jitter* so clients do not line up\n\
         - give up after `MAX_RETRIES` and say why\n\n\
         ```rust\n\
         fn retry<T>(mut op: impl FnMut() -> Result<T, Error>) -> Result<T, Error> {{\n\
         \x20   for attempt in 0..MAX_RETRIES {{\n\
         \x20       match op() {{\n\
         \x20           Ok(value) => return Ok(value),\n\
         \x20           Err(error) if error.is_permanent() => return Err(error),\n\
         \x20           Err(_) => std::thread::sleep(backoff(attempt, {n})),\n\
         \x20       }}\n\
         \x20   }}\n\
         \x20   op()\n\
         }}\n\
         ```\n\n\
         Tests cover the permanent and the transient case; the rest of the suite is unchanged.\n"
    )
}

fn diff(n: usize) -> String {
    let mut text = format!(
        "diff --git a/src/retry_{n}.rs b/src/retry_{n}.rs\n--- a/src/retry_{n}.rs\n+++ b/src/retry_{n}.rs\n@@ -1,14 +1,18 @@\n"
    );
    for i in 0..6 {
        text.push_str(&format!(" fn context_{i}() {{ unchanged(); }}\n"));
    }
    for i in 0..4 {
        text.push_str(&format!("-    old_call_{i}(attempt, {n});\n"));
    }
    for i in 0..8 {
        text.push_str(&format!(
            "+    new_call_{i}(attempt, {n}, backoff(attempt));\n"
        ));
    }
    text.push_str(" }\n");
    text
}

fn fill(out: &Out, turn: &str, units: usize) {
    for n in 0..units {
        let user = format!("u{n}");
        let content = json!([{"type": "text", "text": format!("Please look at the retry handling, round {n}."), "text_elements": []}]);
        let user_item =
            json!({"type": "userMessage", "id": user, "clientId": null, "content": content});
        item_event(out, "item/started", turn, user_item.clone());
        item_event(out, "item/completed", turn, user_item);

        message(out, turn, &format!("m{n}"), &markdown(n), "commentary");

        let command = |status: &str, output: Value, code: Value| {
            json!({"type": "commandExecution", "id": format!("c{n}"), "command": format!("/bin/zsh -lc 'cargo test -p retry_{n}'"),
                   "cwd": "/tmp/hutan-fixture", "source": "agent", "status": status,
                   "commandActions": [{"type": "unknown", "command": format!("cargo test -p retry_{n}")}],
                   "aggregatedOutput": output, "exitCode": code, "durationMs": 1200})
        };
        item_event(
            out,
            "item/started",
            turn,
            command("inProgress", Value::Null, Value::Null),
        );
        let output: String = (0..24)
            .map(|i| format!("test retry_{n}::case_{i} ... ok\n"))
            .collect();
        item_event(
            out,
            "item/completed",
            turn,
            command("completed", json!(output), json!(0)),
        );

        let change = |status: &str| {
            json!({"type": "fileChange", "id": format!("f{n}"), "status": status,
                   "changes": [{"path": format!("/tmp/hutan-fixture/src/retry_{n}.rs"),
                                "kind": {"type": "update", "move_path": null}, "diff": diff(n)}]})
        };
        item_event(out, "item/started", turn, change("inProgress"));
        item_event(out, "item/completed", turn, change("completed"));
    }
}

const WORDS: &[&str] = &[
    "the",
    "handler",
    "retries",
    "every",
    "error",
    "including",
    "those",
    "that",
    "cannot",
    "succeed",
    "so",
    "classify",
    "first",
    "then",
    "back",
    "off",
    "with",
    "jitter",
    "and",
    "stop",
];

fn stream(out: &Out, cancel: &AtomicBool, turn: &str, per_second: u64, seconds: u64) {
    let id = "stream";
    let item = |text: &str| {
        json!({"type": "agentMessage", "id": id, "text": text, "phase": "final_answer",
               "memoryCitation": null, "delivery": null, "questions": null})
    };
    item_event(out, "item/started", turn, item(""));
    let total = per_second * seconds;
    let started = Instant::now();
    let mut full = String::new();
    for n in 0..total {
        if cancel.load(Ordering::SeqCst) {
            break;
        }
        let due = started + Duration::from_micros(n * 1_000_000 / per_second.max(1));
        std::thread::sleep(due.saturating_duration_since(Instant::now()));
        // A paragraph break every 40 tokens, a code block every 400, markdown in between.
        let delta = if n % 400 == 399 {
            "\n\n```rust\nfn main() {\n    println!(\"hello\");\n}\n```\n\n".to_owned()
        } else if n % 40 == 39 {
            ".\n\n".to_owned()
        } else if n % 13 == 5 {
            " `code`".to_owned()
        } else {
            format!(" {}", WORDS[usize::try_from(n).unwrap_or(0) % WORDS.len()])
        };
        full.push_str(&delta);
        notify(
            out,
            "item/agentMessage/delta",
            json!({"threadId": THREAD, "turnId": turn, "itemId": id, "delta": delta}),
        );
    }
    item_event(out, "item/completed", turn, item(&full));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_has_a_code_block_and_a_list() {
        let text = markdown(3);
        assert!(text.contains("```rust"));
        assert!(text.contains("- classify"));
    }

    #[test]
    fn diffs_have_added_and_removed_lines() {
        let text = diff(1);
        assert!(text.lines().any(|l| l.starts_with("+    new_call_")));
        assert!(text.lines().any(|l| l.starts_with("-    old_call_")));
    }
}
