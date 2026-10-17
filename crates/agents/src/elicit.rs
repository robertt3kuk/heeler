//! The form of an elicitation → [`QuestionField`]s, and the answers back into the form's
//! `content` (pure; `docs/agents.md` § Questions).
//!
//! MCP's `elicitation/create` (Codex forwards it as `mcpServer/elicitation/request`) and ACP's
//! `elicitation/create` carry the same JSON-schema subset: an object whose properties are
//! strings (free text, `enum`, `oneOf` of `{const, title}`), numbers, integers, booleans, and
//! arrays of an enum (multi-select). Anything else (a nested object, `$ref`, a schema that is
//! not an object at all, as Codex's `openai/form` mode allows) has no mapping: the question
//! then has one free-text field that takes the answer as JSON, and the schema itself is kept
//! for the user to read.

use hutan_core::question::{
    ChoiceOption, FieldKind, MAX_FIELDS, MAX_OPTIONS, QuestionField, Values, natural_cmp,
};
use serde_json::{Map, Value};

/// Id of the single field of a form that has no mapping.
pub const RAW_FIELD: &str = "response";

/// How an answer is turned back into `content`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// One entry per field, by the field's id.
    Form,
    /// The one [`RAW_FIELD`] holds the whole `content` as JSON.
    Raw,
}

/// A form as the user sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct Elicitation {
    pub fields: Vec<QuestionField>,
    /// The provider's schema, when `fields` is not a faithful rendering of it.
    pub raw_schema: Option<Value>,
    pub shape: Shape,
}

/// Maps a `requestedSchema`. Never fails: what cannot be mapped becomes [`Shape::Raw`].
pub fn parse(schema: &Value) -> Elicitation {
    match fields_of(schema) {
        Some(fields) => Elicitation {
            fields,
            raw_schema: None,
            shape: Shape::Form,
        },
        None => raw(schema),
    }
}

/// A form that is shown as it came: one free-text field that takes the answer as JSON (Codex's
/// `openai/form` modes, whose schema is whatever the server wants, end up here).
pub fn raw(schema: &Value) -> Elicitation {
    Elicitation {
        fields: vec![QuestionField {
            id: RAW_FIELD.to_owned(),
            label: "Response (JSON)".to_owned(),
            description: Some(
                "Hutan cannot draw this form: answer with a JSON object, as the schema asks."
                    .to_owned(),
            ),
            kind: FieldKind::Multiline,
            required: false,
            default: None,
        }],
        raw_schema: Some(schema.clone()),
        shape: Shape::Raw,
    }
}

fn fields_of(schema: &Value) -> Option<Vec<QuestionField>> {
    let object = schema.as_object()?;
    if object
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|t| t != "object")
    {
        return None;
    }
    let properties = match object.get("properties") {
        Some(Value::Object(properties)) => properties,
        None | Some(Value::Null) => return Some(Vec::new()),
        Some(_) => return None,
    };
    if properties.len() > MAX_FIELDS {
        return None;
    }
    let required: Vec<&str> = object
        .get("required")
        .and_then(Value::as_array)
        .map(|names| names.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let mut names: Vec<&String> = properties.keys().collect();
    names.sort_by(|a, b| natural_cmp(a, b));
    names
        .into_iter()
        .map(|name| {
            property(
                name,
                properties.get(name)?,
                required.contains(&name.as_str()),
            )
        })
        .collect()
}

fn text_of<'a>(schema: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    schema
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// `{const, title, description?}` entries (`oneOf` / `anyOf`).
fn titled_options(entries: &Value) -> Option<Vec<ChoiceOption>> {
    let entries = entries.as_array()?;
    if entries.len() > MAX_OPTIONS {
        return None;
    }
    entries
        .iter()
        .map(|entry| {
            let entry = entry.as_object()?;
            let value = entry.get("const")?.as_str()?.to_owned();
            Some(ChoiceOption {
                label: text_of(entry, "title").unwrap_or(&value).to_owned(),
                description: text_of(entry, "description").map(str::to_owned),
                value,
            })
        })
        .collect()
}

/// A plain `enum`, with the legacy `enumNames` as labels.
fn enum_options(schema: &Map<String, Value>, names: &Value) -> Option<Vec<ChoiceOption>> {
    let values = names.as_array()?;
    if values.len() > MAX_OPTIONS {
        return None;
    }
    let labels = schema.get("enumNames").and_then(Value::as_array);
    values
        .iter()
        .enumerate()
        .map(|(index, value)| {
            let value = value.as_str()?.to_owned();
            let label = labels
                .and_then(|labels| labels.get(index))
                .and_then(Value::as_str)
                .unwrap_or(&value)
                .to_owned();
            Some(ChoiceOption {
                value,
                label,
                description: None,
            })
        })
        .collect()
}

fn property(name: &str, schema: &Value, required: bool) -> Option<QuestionField> {
    let schema = schema.as_object()?;
    let mut field = QuestionField {
        id: name.to_owned(),
        label: text_of(schema, "title").unwrap_or(name).to_owned(),
        description: text_of(schema, "description").map(str::to_owned),
        kind: FieldKind::Text,
        required,
        default: schema.get("default").filter(|d| !d.is_null()).cloned(),
    };
    match schema.get("type").and_then(Value::as_str)? {
        "string" => {
            let options = schema
                .get("oneOf")
                .map(titled_options)
                .or_else(|| schema.get("enum").map(|e| enum_options(schema, e)));
            field.kind = match options {
                Some(Some(options)) => FieldKind::Choice {
                    options,
                    multi: false,
                    allow_other: false,
                },
                Some(None) => return None,
                None => {
                    let password = text_of(schema, "format") == Some("password")
                        || schema.get("writeOnly").and_then(Value::as_bool) == Some(true);
                    let long = schema.get("maxLength").and_then(Value::as_u64) > Some(200)
                        || field
                            .default
                            .as_ref()
                            .and_then(Value::as_str)
                            .is_some_and(|d| d.contains('\n'));
                    if password {
                        FieldKind::Secret
                    } else if long {
                        FieldKind::Multiline
                    } else {
                        FieldKind::Text
                    }
                }
            };
            if !matches!(field.default, None | Some(Value::String(_))) {
                field.default = None;
            }
        }
        kind @ ("number" | "integer") => {
            field.kind = FieldKind::Number {
                integer: kind == "integer",
                min: schema.get("minimum").and_then(Value::as_f64),
                max: schema.get("maximum").and_then(Value::as_f64),
            };
            if !field.default.as_ref().is_none_or(Value::is_number) {
                field.default = None;
            }
        }
        "boolean" => {
            field.kind = FieldKind::Boolean;
            if !field.default.as_ref().is_none_or(Value::is_boolean) {
                field.default = None;
            }
        }
        "array" => {
            let items = schema.get("items")?.as_object()?;
            let options = items
                .get("anyOf")
                .or_else(|| items.get("oneOf"))
                .and_then(titled_options)
                .or_else(|| enum_options(items, items.get("enum")?))?;
            field.kind = FieldKind::Choice {
                options,
                multi: true,
                allow_other: false,
            };
            if !field.default.as_ref().is_none_or(|d| {
                d.as_array()
                    .is_some_and(|items| items.iter().all(Value::is_string))
            }) {
                field.default = None;
            }
        }
        _ => return None,
    }
    Some(field)
}

/// The `content` of an accepted elicitation from validated `values`. `Err` is what the user
/// must fix (only a [`Shape::Raw`] answer can be wrong at this point).
pub fn content(shape: Shape, fields: &[QuestionField], values: &Values) -> Result<Value, String> {
    match shape {
        Shape::Form => {
            let mut content = Map::new();
            for field in fields {
                let Some(value) = values.get(&field.id) else {
                    continue;
                };
                let value = match (&field.kind, value) {
                    (FieldKind::Number { integer: true, .. }, Value::Number(n)) => n
                        .as_i64()
                        .map(Value::from)
                        .or_else(|| n.as_f64().map(|f| Value::from(f as i64)))
                        .unwrap_or_else(|| value.clone()),
                    _ => value.clone(),
                };
                content.insert(field.id.clone(), value);
            }
            Ok(Value::Object(content))
        }
        Shape::Raw => match values.get(RAW_FIELD).and_then(Value::as_str) {
            None => Ok(Value::Object(Map::new())),
            Some(text) => match serde_json::from_str::<Value>(text) {
                Ok(content @ Value::Object(_)) => Ok(content),
                Ok(_) => Err("the response must be a JSON object".to_owned()),
                Err(error) => Err(format!("the response is not valid JSON: {error}")),
            },
        },
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_primitive_types_map_to_fields_in_natural_order() {
        let form = parse(&json!({
            "type": "object",
            "required": ["q2"],
            "properties": {
                "q10": {"type": "string", "title": "Ten", "description": "later"},
                "q2": {"type": "string", "oneOf": [
                    {"const": "a", "title": "Alpha", "description": "first"},
                    {"const": "b", "title": "Beta"}
                ], "default": "a"},
                "n": {"type": "integer", "minimum": 1, "maximum": 5, "default": 2},
                "ok": {"type": "boolean", "default": true},
                "pw": {"type": "string", "format": "password"},
                "many": {"type": "array", "items": {"enum": ["x", "y"]}},
                "titled": {"type": "array", "items": {"anyOf": [{"const": "x", "title": "X"}]}},
                "legacy": {"type": "string", "enum": ["l1", "l2"], "enumNames": ["One", "Two"]},
                "essay": {"type": "string", "maxLength": 2000}
            }
        }));
        assert_eq!(form.shape, Shape::Form);
        assert!(form.raw_schema.is_none());
        let ids: Vec<&str> = form.fields.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "essay", "legacy", "many", "n", "ok", "pw", "q2", "q10", "titled"
            ]
        );
        let by = |id: &str| form.fields.iter().find(|f| f.id == id).unwrap();
        assert!(matches!(by("essay").kind, FieldKind::Multiline));
        assert!(matches!(by("pw").kind, FieldKind::Secret));
        assert!(by("q2").required);
        assert_eq!(by("q2").default, Some(json!("a")));
        let FieldKind::Choice { options, multi, .. } = &by("q2").kind else {
            panic!("q2 is a choice");
        };
        assert!(!multi);
        assert_eq!(options[0].label, "Alpha");
        assert_eq!(options[0].description.as_deref(), Some("first"));
        let FieldKind::Choice { options, .. } = &by("legacy").kind else {
            panic!("legacy is a choice");
        };
        assert_eq!(options[1].label, "Two");
        assert!(matches!(
            by("many").kind,
            FieldKind::Choice { multi: true, .. }
        ));
        assert!(matches!(
            by("n").kind,
            FieldKind::Number {
                integer: true,
                min: Some(_),
                max: Some(_)
            }
        ));
        assert_eq!(by("q10").label, "Ten");
        assert_eq!(by("q10").description.as_deref(), Some("later"));
    }

    #[test]
    fn a_form_without_properties_has_no_fields() {
        let form = parse(&json!({"type": "object", "properties": {}}));
        assert_eq!(form.shape, Shape::Form);
        assert!(form.fields.is_empty());
    }

    #[test]
    fn unknown_shapes_become_one_free_text_field_beside_the_raw_schema() {
        for schema in [
            json!(true),
            json!({"type": "object", "properties": {"n": {"type": "object"}}}),
            json!({"type": "object", "properties": {"r": {"$ref": "#/x"}}}),
            json!({"type": "array"}),
        ] {
            let form = parse(&schema);
            assert_eq!(form.shape, Shape::Raw, "{schema}");
            assert_eq!(form.fields.len(), 1);
            assert_eq!(form.fields[0].id, RAW_FIELD);
            assert_eq!(form.raw_schema.as_ref(), Some(&schema));
        }
    }

    #[test]
    fn content_follows_the_field_types() {
        let form = parse(&json!({"type": "object", "properties": {
            "n": {"type": "integer"}, "ok": {"type": "boolean"}, "t": {"type": "string"}
        }}));
        let values: Values = [
            ("n".to_owned(), json!(3.0)),
            ("ok".to_owned(), json!(true)),
            ("t".to_owned(), json!("hi")),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            content(form.shape, &form.fields, &values).unwrap(),
            json!({"n": 3, "ok": true, "t": "hi"})
        );
    }

    #[test]
    fn a_raw_answer_must_be_a_json_object() {
        let form = parse(&json!(true));
        let answer =
            |text: &str| -> Values { [(RAW_FIELD.to_owned(), json!(text))].into_iter().collect() };
        assert_eq!(
            content(form.shape, &form.fields, &answer("{\"a\": 1}")).unwrap(),
            json!({"a": 1})
        );
        assert!(content(form.shape, &form.fields, &answer("[1]")).is_err());
        assert!(content(form.shape, &form.fields, &answer("nope")).is_err());
        assert_eq!(
            content(form.shape, &form.fields, &Values::new()).unwrap(),
            json!({})
        );
    }
}
