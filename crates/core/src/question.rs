//! Questions an agent puts to the user (protocol 0.24, capability `chat.questions`): the
//! form model, the answer, and the validation of one against the other. No I/O lives here.
//!
//! A question is a small form: a markdown `prompt` and typed [`QuestionField`]s that the
//! providers' JSON-schema subsets (Codex `requestUserInput`, MCP elicitation, ACP
//! `elicitation/create`) map onto. A shape that has no mapping becomes one free-text field next
//! to the raw schema. Questions live in daemon state like approvals: they survive client
//! disconnects and expire with their turn or process; every decision says who made it.
//!
//! Secret fields are the one place where an answer is more than data: the value goes to the
//! agent and nowhere else. [`QuestionOutcome::answered`] withholds it from what the journal,
//! the audit table and the events carry.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Answers by field id. A value is a JSON string, number, boolean, or array of strings,
/// matching the field's kind ([`validate`]).
pub type Values = BTreeMap<String, Value>;

/// Most fields one question has; a provider form with more is cut (the rest is in
/// `raw_schema`).
pub const MAX_FIELDS: usize = 32;
/// Most options one choice field has.
pub const MAX_OPTIONS: usize = 100;
/// Longest accepted answer text, in bytes (per field).
pub const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// One option of a choice field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChoiceOption {
    /// What the answer holds when this option is chosen (the provider's own identifier).
    pub value: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// What a field takes. Internally tagged by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    /// One line of text.
    Text,
    /// Free text of several lines.
    Multiline,
    /// One option (a string) or, with `multi`, several (an array of strings). `allow_other`:
    /// any string is accepted, not only the options.
    Choice {
        options: Vec<ChoiceOption>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        multi: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        allow_other: bool,
    },
    Number {
        /// Whole numbers only.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        integer: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
    },
    Boolean,
    /// A password or token: typed masked, sent to the agent, never journaled or logged.
    Secret,
}

/// One field of a question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionField {
    /// The provider's key of the field; the key of its answer.
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: FieldKind,
    #[serde(default)]
    pub required: bool,
    /// Pre-filled value: the same JSON type an answer has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

impl QuestionField {
    /// The field takes a secret (its answer is never journaled).
    pub fn is_secret(&self) -> bool {
        matches!(self.kind, FieldKind::Secret)
    }
}

/// What a question is bound to: when it can no longer be answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuestionExpiry {
    /// The turn that asked: when it ends the question is over (like an approval).
    #[default]
    Turn,
    /// The agent's process: asked outside any turn (an MCP server's elicitation); it ends with
    /// the process, so with the daemon.
    Process,
}

/// What an agent asks, without the identity the daemon gives it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QuestionSpec {
    pub title: String,
    /// Markdown; may be empty when the fields say it all.
    #[serde(default)]
    pub prompt: String,
    #[serde(default)]
    pub fields: Vec<QuestionField>,
    /// The provider's schema when it had no mapping onto fields (then `fields` is one
    /// free-text field) or when fields were cut: shown to the user as it came.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_schema: Option<Value>,
    /// Who asks when it is not the agent itself (`MCP server linear`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default)]
    pub expires: QuestionExpiry,
}

/// What the user decided. `Debug` names the fields that were answered and nothing they hold: a
/// value may be a secret, and requests are printed by logs and test failures.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum QuestionResponse {
    /// The values of the fields (see [`validate`]).
    Submit { values: Values },
    /// No answer, and the agent goes on without one.
    Decline,
    /// No answer, and the turn stops where the provider has that distinction.
    Cancel,
}

impl std::fmt::Debug for QuestionResponse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Submit { values } => f
                .debug_struct("Submit")
                .field("fields", &values.keys().collect::<Vec<_>>())
                .finish_non_exhaustive(),
            Self::Decline => f.write_str("Decline"),
            Self::Cancel => f.write_str("Cancel"),
        }
    }
}

/// How a question ended. Flattened into the events that report it (`outcome` tags it).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum QuestionOutcome {
    /// Answered. `values` holds the answers of every field except the secret ones, which the
    /// agent got and nobody else keeps: `withheld` lists their ids ("answered (secret)").
    Answered {
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        values: Values,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        withheld: Vec<String>,
    },
    Declined,
    Cancelled,
    /// The turn ended, the agent went away or the daemon restarted before anyone answered.
    Expired,
}

impl QuestionOutcome {
    /// The outcome of an answer: `values` minus what secret fields hold.
    pub fn answered(fields: &[QuestionField], values: &Values) -> Self {
        let mut kept = Values::new();
        let mut withheld = Vec::new();
        for (id, value) in values {
            if fields.iter().any(|f| f.id == *id && f.is_secret()) {
                withheld.push(id.clone());
            } else {
                kept.insert(id.clone(), value.clone());
            }
        }
        Self::Answered {
            values: kept,
            withheld,
        }
    }

    /// A short word for logs and the audit table (never a value).
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Answered { .. } => "answered",
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }
}

/// Why an answer does not fit its question.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AnswerError {
    #[error("the question has no field `{0}`")]
    UnknownField(String),
    #[error("`{0}` is required")]
    Missing(String),
    #[error("`{field}`: {problem}")]
    Invalid { field: String, problem: String },
}

fn invalid(field: &QuestionField, problem: impl Into<String>) -> AnswerError {
    AnswerError::Invalid {
        field: field.id.clone(),
        problem: problem.into(),
    }
}

/// Checks `values` against `fields` and returns what to send: unknown fields and wrong types are
/// refused, a field left out takes its default, a required one that has neither is refused.
/// An optional text field left empty is left out.
pub fn validate(fields: &[QuestionField], values: &Values) -> Result<Values, AnswerError> {
    for id in values.keys() {
        if !fields.iter().any(|f| f.id == *id) {
            return Err(AnswerError::UnknownField(id.clone()));
        }
    }
    let mut out = Values::new();
    for field in fields {
        let given = values.get(&field.id).or(field.default.as_ref());
        let Some(value) = given.filter(|v| !v.is_null()) else {
            if field.required {
                return Err(AnswerError::Missing(field.id.clone()));
            }
            continue;
        };
        let value = check(field, value)?;
        let empty = value.as_str().is_some_and(str::is_empty)
            || value.as_array().is_some_and(Vec::is_empty);
        if empty {
            if field.required {
                return Err(AnswerError::Missing(field.id.clone()));
            }
            continue;
        }
        out.insert(field.id.clone(), value);
    }
    Ok(out)
}

fn check(field: &QuestionField, value: &Value) -> Result<Value, AnswerError> {
    let text = |value: &Value| -> Result<String, AnswerError> {
        let text = value
            .as_str()
            .ok_or_else(|| invalid(field, "expects text"))?;
        if text.len() > MAX_ANSWER_BYTES {
            return Err(invalid(field, "the text is too long"));
        }
        Ok(text.to_owned())
    };
    match &field.kind {
        FieldKind::Text | FieldKind::Multiline | FieldKind::Secret => {
            text(value).map(Value::String)
        }
        FieldKind::Boolean => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| invalid(field, "expects true or false")),
        FieldKind::Number { integer, min, max } => {
            let number = value
                .as_f64()
                .ok_or_else(|| invalid(field, "expects a number"))?;
            if *integer && number.fract() != 0.0 {
                return Err(invalid(field, "expects a whole number"));
            }
            if min.is_some_and(|min| number < min) {
                return Err(invalid(
                    field,
                    format!("is below the minimum {}", min.unwrap_or_default()),
                ));
            }
            if max.is_some_and(|max| number > max) {
                return Err(invalid(
                    field,
                    format!("is above the maximum {}", max.unwrap_or_default()),
                ));
            }
            Ok(value.clone())
        }
        FieldKind::Choice {
            options,
            multi,
            allow_other,
        } => {
            let known = |choice: &str| options.iter().any(|o| o.value == choice);
            if *multi {
                let items = value
                    .as_array()
                    .ok_or_else(|| invalid(field, "expects a list of options"))?;
                let mut chosen = Vec::with_capacity(items.len());
                for item in items {
                    let item = text(item)?;
                    if !allow_other && !known(&item) {
                        return Err(invalid(field, format!("`{item}` is not an option")));
                    }
                    chosen.push(Value::String(item));
                }
                Ok(Value::Array(chosen))
            } else {
                let chosen = text(value)?;
                if !allow_other && !chosen.is_empty() && !known(&chosen) {
                    return Err(invalid(field, format!("`{chosen}` is not an option")));
                }
                Ok(Value::String(chosen))
            }
        }
    }
}

/// Orders property names the way a person numbers them (`q2` before `q10`): JSON objects arrive
/// without the order the provider wrote them in.
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let run = |it: &mut std::iter::Peekable<std::str::Chars<'_>>| {
                    let mut digits = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        digits.push(c);
                        it.next();
                    }
                    digits.trim_start_matches('0').to_owned()
                };
                let (x, y) = (run(&mut a), run(&mut b));
                let order = x.len().cmp(&y.len()).then_with(|| x.cmp(&y));
                if order.is_ne() {
                    return order;
                }
            }
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
                a.next();
                b.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn text(id: &str, required: bool) -> QuestionField {
        QuestionField {
            id: id.into(),
            label: id.into(),
            description: None,
            kind: FieldKind::Text,
            required,
            default: None,
        }
    }

    fn choice(id: &str, multi: bool, other: bool) -> QuestionField {
        QuestionField {
            id: id.into(),
            label: id.into(),
            description: None,
            kind: FieldKind::Choice {
                options: vec![
                    ChoiceOption {
                        value: "a".into(),
                        label: "A".into(),
                        description: None,
                    },
                    ChoiceOption {
                        value: "b".into(),
                        label: "B".into(),
                        description: Some("second".into()),
                    },
                ],
                multi,
                allow_other: other,
            },
            required: false,
            default: None,
        }
    }

    fn values(pairs: &[(&str, Value)]) -> Values {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn fields_round_trip_with_a_nested_kind() {
        let field = choice("scope", true, false);
        let value = serde_json::to_value(&field).unwrap();
        assert_eq!(value["kind"]["type"], "choice");
        assert_eq!(value["kind"]["multi"], true);
        assert!(value["kind"].get("allow_other").is_none());
        assert_eq!(
            serde_json::from_value::<QuestionField>(value).unwrap(),
            field
        );
    }

    #[test]
    fn outcomes_flatten_and_withhold_secrets() {
        let fields = vec![
            text("user", true),
            QuestionField {
                kind: FieldKind::Secret,
                ..text("token", true)
            },
        ];
        let answer = values(&[("user", json!("ada")), ("token", json!("hunter2"))]);
        let outcome = QuestionOutcome::answered(&fields, &answer);
        let wire = serde_json::to_string(&outcome).unwrap();
        assert!(!wire.contains("hunter2"), "{wire}");
        assert_eq!(
            serde_json::to_value(&outcome).unwrap(),
            json!({"outcome": "answered", "values": {"user": "ada"}, "withheld": ["token"]})
        );
        assert_eq!(
            serde_json::to_value(QuestionOutcome::Expired).unwrap(),
            json!({"outcome": "expired"})
        );
    }

    #[test]
    fn a_response_never_prints_what_it_holds() {
        let submit = QuestionResponse::Submit {
            values: values(&[("token", json!("hunter2")), ("user", json!("ada"))]),
        };
        let printed = format!("{submit:?}");
        assert!(
            !printed.contains("hunter2") && !printed.contains("ada"),
            "{printed}"
        );
        assert!(printed.contains("token"), "{printed}");
    }

    #[test]
    fn responses_are_tagged_by_action() {
        let submit = QuestionResponse::Submit {
            values: values(&[("a", json!("x"))]),
        };
        assert_eq!(
            serde_json::to_value(&submit).unwrap(),
            json!({"action": "submit", "values": {"a": "x"}})
        );
        assert_eq!(
            serde_json::from_value::<QuestionResponse>(json!({"action": "decline"})).unwrap(),
            QuestionResponse::Decline
        );
    }

    #[test]
    fn validation_applies_defaults_and_requires() {
        let mut with_default = text("name", true);
        with_default.default = Some(json!("anon"));
        let fields = vec![with_default, text("note", false), text("must", true)];
        assert_eq!(
            validate(&fields, &Values::new()),
            Err(AnswerError::Missing("must".into()))
        );
        let ok = validate(
            &fields,
            &values(&[("must", json!("yes")), ("note", json!(""))]),
        )
        .unwrap();
        assert_eq!(
            ok,
            values(&[("name", json!("anon")), ("must", json!("yes"))])
        );
        assert_eq!(
            validate(&fields, &values(&[("extra", json!(1))])),
            Err(AnswerError::UnknownField("extra".into()))
        );
    }

    #[test]
    fn validation_checks_types_and_options() {
        let one = |field: &QuestionField, value: Value| {
            validate(std::slice::from_ref(field), &values(&[(&field.id, value)]))
        };
        let strict = choice("c", false, false);
        assert!(one(&strict, json!("a")).is_ok());
        assert!(matches!(
            one(&strict, json!("z")),
            Err(AnswerError::Invalid { .. })
        ));
        assert!(one(&choice("c", false, true), json!("anything")).is_ok());
        let multi = choice("m", true, false);
        assert!(one(&multi, json!(["a", "b"])).is_ok());
        assert!(one(&multi, json!("a")).is_err());

        let number = QuestionField {
            kind: FieldKind::Number {
                integer: true,
                min: Some(1.0),
                max: Some(10.0),
            },
            ..text("n", false)
        };
        assert!(one(&number, json!(5)).is_ok());
        assert!(one(&number, json!(5.5)).is_err());
        assert!(one(&number, json!(0)).is_err());
        assert!(one(&number, json!(11)).is_err());
        assert!(one(&number, json!("5")).is_err());

        let flag = QuestionField {
            kind: FieldKind::Boolean,
            ..text("b", false)
        };
        assert!(one(&flag, json!(false)).is_ok());
        assert!(one(&flag, json!("false")).is_err());
    }

    #[test]
    fn natural_order_numbers_runs() {
        let mut names = vec!["q10", "q2", "q1", "q1__other", "q0"];
        names.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(names, vec!["q0", "q1", "q1__other", "q2", "q10"]);
    }
}
