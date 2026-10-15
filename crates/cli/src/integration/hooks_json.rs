//! Editing the `hooks` table of Codex's `hooks.json` and Claude Code's `settings.json`.
//!
//! Both files share one shape: `{"hooks": {"<Event>": [{"hooks": [{"type": "command", ...}]}]}}`.
//! Hutan adds one group per event and recognises its own hooks by [`MARKER`] in the command;
//! every other group, hook, and key is left exactly as it was (the documents keep their key
//! order and are written back with the indentation they came with).

use serde_json::{Map, Value, json};

/// Appears in every command Hutan installs: `: hutan-integration=<target>; sh '<script>' ...`.
pub const MARKER: &str = "hutan-integration=";

/// Seconds the agent lets a hook run.
const HOOK_TIMEOUT_SECS: u64 = 10;

/// Events whose hook runs in the background (`"async": true`) so that it never delays the tool
/// call or the model switch it reports on.
const ASYNC_EVENTS: &[&str] = &["PreToolUse", "PostToolUseFailure", "PostModelSwitch"];

#[derive(Debug, thiserror::Error)]
pub enum EditError {
    #[error("the top level is not a JSON object")]
    NotAnObject,
    #[error("`{0}` is not the shape the agent documents (an object or array was expected)")]
    UnexpectedShape(String),
}

/// Makes sure every event in `events` has a Hutan hook running `command`, and that no other
/// event has one. Returns whether the document changed.
pub fn install(doc: &mut Value, events: &[&str], command: &str) -> Result<bool, EditError> {
    let root = doc.as_object_mut().ok_or(EditError::NotAnObject)?;
    let mut changed = false;
    if !root.contains_key("hooks") {
        root.insert("hooks".to_owned(), json!({}));
        changed = true;
    }
    let hooks = root
        .get_mut("hooks")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| EditError::UnexpectedShape("hooks".to_owned()))?;

    // Events an older Hutan hooked and this one does not.
    let stale: Vec<String> = hooks
        .keys()
        .filter(|event| !events.contains(&event.as_str()))
        .cloned()
        .collect();
    for event in stale {
        if let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut)
            && strip_ours(groups)
        {
            changed = true;
            drop_if_emptied(hooks, &event);
        }
    }

    for event in events {
        if !hooks.contains_key(*event) {
            hooks.insert((*event).to_owned(), json!([]));
        }
        let groups = hooks
            .get_mut(*event)
            .and_then(Value::as_array_mut)
            .ok_or_else(|| EditError::UnexpectedShape(format!("hooks.{event}")))?;
        match find_ours(groups) {
            Some(hook) => {
                if hook.get("command").and_then(Value::as_str) != Some(command) {
                    hook.insert("command".to_owned(), json!(command));
                    changed = true;
                }
                if ASYNC_EVENTS.contains(event) != (hook.get("async") == Some(&json!(true))) {
                    if ASYNC_EVENTS.contains(event) {
                        hook.insert("async".to_owned(), json!(true));
                    } else {
                        hook.remove("async");
                    }
                    changed = true;
                }
            }
            None => {
                let mut hook =
                    json!({"type": "command", "command": command, "timeout": HOOK_TIMEOUT_SECS});
                if ASYNC_EVENTS.contains(event) {
                    hook["async"] = json!(true);
                }
                groups.push(json!({ "hooks": [hook] }));
                changed = true;
            }
        }
    }
    Ok(changed)
}

/// Removes Hutan's hooks, and the groups, events, and `hooks` table they leave empty.
/// Returns whether the document changed.
pub fn uninstall(doc: &mut Value) -> Result<bool, EditError> {
    let root = doc.as_object_mut().ok_or(EditError::NotAnObject)?;
    let Some(hooks) = root.get_mut("hooks") else {
        return Ok(false);
    };
    let hooks = hooks
        .as_object_mut()
        .ok_or_else(|| EditError::UnexpectedShape("hooks".to_owned()))?;
    let mut changed = false;
    let events: Vec<String> = hooks.keys().cloned().collect();
    for event in events {
        if let Some(groups) = hooks.get_mut(&event).and_then(Value::as_array_mut)
            && strip_ours(groups)
        {
            changed = true;
            drop_if_emptied(hooks, &event);
        }
    }
    if changed && hooks.is_empty() {
        root.remove("hooks");
    }
    Ok(changed)
}

/// The events of `events` that have a Hutan hook.
pub fn installed_events<'a>(doc: &Value, events: &[&'a str]) -> Vec<&'a str> {
    let Some(hooks) = doc.get("hooks").and_then(Value::as_object) else {
        return Vec::new();
    };
    events
        .iter()
        .copied()
        .filter(|event| {
            hooks
                .get(*event)
                .and_then(Value::as_array)
                .is_some_and(|groups| groups.iter().any(group_has_ours))
        })
        .collect()
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(Value::as_str)
        .is_some_and(|command| command.contains(MARKER))
}

fn group_has_ours(group: &Value) -> bool {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .is_some_and(|hooks| hooks.iter().any(is_ours))
}

fn find_ours(groups: &mut [Value]) -> Option<&mut Map<String, Value>> {
    groups
        .iter_mut()
        .filter_map(|group| group.get_mut("hooks").and_then(Value::as_array_mut))
        .flat_map(|hooks| hooks.iter_mut())
        .filter(|hook| is_ours(hook))
        .find_map(Value::as_object_mut)
}

/// Removes our hooks from the groups, and the groups that held nothing else. Returns whether
/// anything was removed.
fn strip_ours(groups: &mut Vec<Value>) -> bool {
    let mut removed = false;
    groups.retain_mut(|group| {
        let Some(hooks) = group.get_mut("hooks").and_then(Value::as_array_mut) else {
            return true;
        };
        let before = hooks.len();
        hooks.retain(|hook| !is_ours(hook));
        let stripped = hooks.len() != before;
        let emptied = hooks.is_empty();
        removed |= stripped;
        // A group we created holds only our hook; one with a `matcher` or other keys is the
        // user's and stays even when nothing is left in it.
        !(stripped && emptied && group.as_object().is_some_and(|keys| keys.len() == 1))
    });
    removed
}

/// Drops the event key when it holds no groups. Returns whether it did.
fn drop_if_emptied(hooks: &mut Map<String, Value>, event: &str) -> bool {
    let empty = hooks
        .get(event)
        .and_then(Value::as_array)
        .is_some_and(Vec::is_empty);
    if empty {
        hooks.remove(event);
    }
    empty
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = ": hutan-integration=codex; sh '/h/.codex/hutan-agent-state.sh' codex";

    fn doc(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    #[test]
    fn install_adds_one_group_per_event_and_keeps_the_rest() {
        let mut value = doc(
            r#"{"model":"x","hooks":{"Stop":[{"hooks":[{"type":"command","command":"theirs"}]}]}}"#,
        );
        assert!(install(&mut value, &["Stop", "SessionStart"], CMD).unwrap());
        assert_eq!(
            value,
            doc(&format!(
                r#"{{"model":"x","hooks":{{
                    "Stop":[
                        {{"hooks":[{{"type":"command","command":"theirs"}}]}},
                        {{"hooks":[{{"type":"command","command":"{CMD}","timeout":10}}]}}],
                    "SessionStart":[
                        {{"hooks":[{{"type":"command","command":"{CMD}","timeout":10}}]}}]}}}}"#
            ))
        );
        // Idempotent.
        assert!(!install(&mut value, &["Stop", "SessionStart"], CMD).unwrap());
    }

    #[test]
    fn install_updates_a_changed_command_in_place_and_drops_retired_events() {
        let mut value = doc(r#"{}"#);
        install(
            &mut value,
            &["Stop", "Old"],
            ": hutan-integration=codex; old",
        )
        .unwrap();
        assert!(install(&mut value, &["Stop"], CMD).unwrap());
        assert_eq!(
            value,
            doc(&format!(
                r#"{{"hooks":{{"Stop":[{{"hooks":[{{"type":"command","command":"{CMD}","timeout":10}}]}}]}}}}"#
            ))
        );
    }

    #[test]
    fn uninstall_restores_the_original_document() {
        let originals = [
            r#"{}"#,
            r#"{"model":"x"}"#,
            r#"{"hooks":{}}"#,
            r#"{"a":1,"hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"theirs"}]}],"Other":[]}}"#,
        ];
        for original in originals {
            let mut value = doc(original);
            install(&mut value, &["Stop", "SessionStart"], CMD).unwrap();
            assert_ne!(value, doc(original));
            assert!(uninstall(&mut value).unwrap());
            // `{"hooks":{}}` and a missing table are the same thing to the agent, and
            // uninstalling removes the table only if installing created it.
            let expected = if original == r#"{"hooks":{}}"# {
                doc(r#"{}"#)
            } else {
                doc(original)
            };
            assert_eq!(value, expected, "{original}");
            assert!(!uninstall(&mut value).unwrap());
        }
    }

    #[test]
    fn a_group_that_shares_our_hook_with_others_keeps_the_others() {
        let mut value = doc(&format!(
            r#"{{"hooks":{{"Stop":[{{"matcher":"*","hooks":[
                {{"type":"command","command":"theirs"}},
                {{"type":"command","command":"{CMD}"}}]}}]}}}}"#
        ));
        assert!(uninstall(&mut value).unwrap());
        assert_eq!(
            value,
            doc(
                r#"{"hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"theirs"}]}]}}"#
            )
        );
    }

    #[test]
    fn installed_events_lists_what_has_our_hook() {
        let mut value = doc(r#"{"hooks":{"Stop":[{"hooks":[{"command":"theirs"}]}]}}"#);
        assert!(installed_events(&value, &["Stop", "SessionStart"]).is_empty());
        install(&mut value, &["SessionStart"], CMD).unwrap();
        assert_eq!(
            installed_events(&value, &["Stop", "SessionStart"]),
            ["SessionStart"]
        );
    }

    #[test]
    fn unexpected_shapes_are_errors_not_overwrites() {
        let mut value = doc(r#"[]"#);
        assert!(matches!(
            install(&mut value, &["Stop"], CMD),
            Err(EditError::NotAnObject)
        ));
        let mut value = doc(r#"{"hooks":[]}"#);
        assert!(matches!(
            install(&mut value, &["Stop"], CMD),
            Err(EditError::UnexpectedShape(_))
        ));
        let mut value = doc(r#"{"hooks":{"Stop":{}}}"#);
        assert!(matches!(
            install(&mut value, &["Stop"], CMD),
            Err(EditError::UnexpectedShape(_))
        ));
    }
}
