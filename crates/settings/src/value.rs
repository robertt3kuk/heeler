//! Pure helpers over `toml::Value` / `serde_json::Value`: deep merge, dotted lookup, conversion,
//! unknown-key detection.

use serde_json::Value as Json;
use toml::Value;
use toml::map::Map;

/// Tables merge key by key (recursively); everything else (scalars, arrays) in `over` replaces.
pub(crate) fn deep_merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Table(base), Value::Table(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(existing) => deep_merge(existing, value),
                    None => {
                        base.insert(key.clone(), value.clone());
                    }
                }
            }
        }
        (base, over) => *base = over.clone(),
    }
}

/// Looks up a dotted namespace path (`panels.sessions`) inside a table.
pub(crate) fn subtree<'a>(table: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut segments = path.split('.');
    let mut current = table.get(segments.next()?)?;
    for segment in segments {
        current = current.as_table()?.get(segment)?;
    }
    Some(current)
}

/// Converts JSON to TOML. `null` has no TOML form: object members holding it are dropped
/// (an unset `Option`), array elements holding it make the array unrepresentable.
pub(crate) fn json_to_toml(json: &Json) -> Option<Value> {
    Some(match json {
        Json::Null => return None,
        Json::Bool(b) => Value::Boolean(*b),
        Json::Number(n) => match (n.as_i64(), n.as_f64()) {
            (Some(i), _) => Value::Integer(i),
            (None, Some(f)) => Value::Float(f),
            (None, None) => return None,
        },
        Json::String(s) => Value::String(s.clone()),
        Json::Array(items) => {
            Value::Array(items.iter().map(json_to_toml).collect::<Option<Vec<_>>>()?)
        }
        Json::Object(map) => Value::Table(
            map.iter()
                .filter_map(|(k, v)| json_to_toml(v).map(|v| (k.clone(), v)))
                .collect(),
        ),
    })
}

/// Paths (relative, dotted) of keys present in `input` that the typed value serialized as `known`
/// does not contain: i.e. keys serde silently ignored.
pub(crate) fn unknown_keys(input: &Value, known: &Json) -> Vec<String> {
    let mut out = Vec::new();
    collect_unknown("", input, known, &mut out);
    out
}

fn collect_unknown(prefix: &str, input: &Value, known: &Json, out: &mut Vec<String>) {
    let (Value::Table(table), Json::Object(known)) = (input, known) else {
        return;
    };
    for (key, value) in table {
        let path = join(prefix, key);
        match known.get(key) {
            None => out.push(path),
            Some(known) => collect_unknown(&path, value, known, out),
        }
    }
}

pub(crate) fn join(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        key.to_owned()
    } else {
        format!("{prefix}.{key}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Value {
        Value::Table(toml::from_str(text).unwrap())
    }

    #[test]
    fn merge_is_deep_for_tables_and_replaces_arrays_and_scalars() {
        let mut base = parse("a = 1\nlist = [1, 2]\n[t]\nx = 1\ny = 2\n");
        deep_merge(&mut base, &parse("a = 2\nlist = [3]\n[t]\ny = 9\nz = 3\n"));
        assert_eq!(base, parse("a = 2\nlist = [3]\n[t]\nx = 1\ny = 9\nz = 3\n"));
    }

    #[test]
    fn subtree_follows_dotted_paths() {
        let table = parse("[panels.sessions]\nrow = 1\n");
        let table = table.as_table().unwrap();
        assert!(subtree(table, "panels.sessions").is_some());
        assert!(subtree(table, "panels.other").is_none());
        assert!(subtree(table, "panels.sessions.row.deeper").is_none());
    }

    #[test]
    fn json_nulls_are_dropped_from_objects() {
        let json = serde_json::json!({"a": 1, "b": null, "c": {"d": null, "e": 2.5}});
        assert_eq!(json_to_toml(&json), Some(parse("a = 1\n[c]\ne = 2.5\n")));
    }

    #[test]
    fn unknown_keys_are_nested_paths() {
        let input = parse("a = 1\nzzz = 2\n[t]\nx = 1\nbogus = 2\n");
        let known = serde_json::json!({"a": 1, "t": {"x": 1}});
        assert_eq!(unknown_keys(&input, &known), vec!["t.bogus", "zzz"]);
    }
}
