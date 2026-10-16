//! Turns the settings store's JSON schemas into editable field descriptions for the settings UI.
//! Pure data: no GPUI.

use std::collections::BTreeMap;
use std::rc::Rc;

use hutan_settings::{AppearanceSettings, Setting, SettingsStore};
use serde_json::Value as Json;

use crate::settings::{
    AgentsSettings, AppSettings, AssistantSettings, ChatSettings, HistorySettings,
    InspectorPanelSettings, LayoutSettings, OverviewPanelSettings, ResourcesSettings,
    SessionsPanelSettings, TerminalSettings, WorkspacesSettings,
};

/// How a field is edited.
#[derive(Debug, Clone, PartialEq)]
pub enum FieldKind {
    Bool,
    /// One of these strings (a dropdown).
    Enum(Vec<String>),
    Integer,
    Number,
    String,
    StringList,
    /// Shown read-only with this hint (maps and other shapes the UI cannot edit).
    Unsupported(String),
}

/// One editable key of a namespace.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Key path inside the namespace (`["cursor", "style"]`).
    pub path: Vec<String>,
    pub doc: String,
    pub kind: FieldKind,
}

impl Field {
    /// `cursor.style`
    pub fn dotted(&self) -> String {
        self.path.join(".")
    }
}

fn resolve<'a>(root: &'a Json, schema: &'a Json) -> &'a Json {
    let mut current = schema;
    // `$ref` chains (and the `allOf: [{ $ref }]` wrapper schemars uses to keep a doc comment).
    for _ in 0..8 {
        if let Some(target) = current
            .get("$ref")
            .and_then(Json::as_str)
            .and_then(|r| r.strip_prefix("#/"))
        {
            let mut node = root;
            for part in target.split('/') {
                node = match node.get(part) {
                    Some(next) => next,
                    None => return current,
                };
            }
            current = node;
        } else if let Some([only]) = current
            .get("allOf")
            .and_then(Json::as_array)
            .map(Vec::as_slice)
        {
            current = only;
        } else {
            break;
        }
    }
    current
}

fn doc_of(schema: &Json, resolved: &Json) -> String {
    schema
        .get("description")
        .or_else(|| resolved.get("description"))
        .and_then(Json::as_str)
        .unwrap_or_default()
        .trim()
        .to_owned()
}

/// String constants of an enum schema (`enum: [...]`, or `oneOf: [{ const }]`).
fn enum_values(schema: &Json) -> Option<Vec<String>> {
    if let Some(values) = schema.get("enum").and_then(Json::as_array) {
        let out: Vec<String> = values
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        return (out.len() == values.len() && !out.is_empty()).then_some(out);
    }
    let variants = schema
        .get("oneOf")
        .or_else(|| schema.get("anyOf"))
        .and_then(Json::as_array)?;
    let mut out = Vec::new();
    for variant in variants {
        if let Some(c) = variant.get("const").and_then(Json::as_str) {
            out.push(c.to_owned());
        } else {
            let values = variant.get("enum").and_then(Json::as_array)?;
            out.extend(values.iter().filter_map(|v| v.as_str().map(str::to_owned)));
        }
    }
    (!out.is_empty()).then_some(out)
}

/// The primary JSON type of a schema (`["string", "null"]` counts as `string`).
fn primary_type(schema: &Json) -> Option<&str> {
    match schema.get("type")? {
        Json::String(t) => Some(t.as_str()),
        Json::Array(types) => types.iter().filter_map(Json::as_str).find(|t| *t != "null"),
        _ => None,
    }
}

fn classify(root: &Json, schema: &Json) -> FieldKind {
    if let Some(values) = enum_values(schema) {
        return FieldKind::Enum(values);
    }
    match primary_type(schema) {
        Some("boolean") => FieldKind::Bool,
        Some("integer") => FieldKind::Integer,
        Some("number") => FieldKind::Number,
        Some("string") => FieldKind::String,
        Some("array") => {
            let items = schema.get("items").map(|i| resolve(root, i));
            match items.and_then(primary_type) {
                Some("string") => FieldKind::StringList,
                _ => FieldKind::Unsupported("edit settings.toml".to_owned()),
            }
        }
        _ => FieldKind::Unsupported("edit settings.toml".to_owned()),
    }
}

fn collect(root: &Json, schema: &Json, prefix: &mut Vec<String>, out: &mut Vec<Field>) {
    let resolved = resolve(root, schema);
    let Some(properties) = resolved.get("properties").and_then(Json::as_object) else {
        return;
    };
    for (key, property) in properties {
        let target = resolve(root, property);
        prefix.push(key.clone());
        let nested = target
            .get("properties")
            .and_then(Json::as_object)
            .is_some_and(|p| !p.is_empty());
        if nested {
            collect(root, property, prefix, out);
        } else {
            let kind = if target
                .get("additionalProperties")
                .is_some_and(Json::is_object)
            {
                FieldKind::Unsupported("edit settings.toml".to_owned())
            } else {
                classify(root, target)
            };
            out.push(Field {
                path: prefix.clone(),
                doc: doc_of(property, target),
                kind,
            });
        }
        prefix.pop();
    }
}

/// Every editable field of a namespace's JSON schema, in declaration order.
pub fn fields_of(schema: &Json) -> Vec<Field> {
    let mut out = Vec::new();
    collect(schema, schema, &mut Vec::new(), &mut out);
    out
}

/// The value at `path` inside `value`.
pub fn value_at<'a>(value: &'a Json, path: &[String]) -> Option<&'a Json> {
    path.iter().try_fold(value, |node, key| node.get(key))
}

/// The TOML value to write for `json` (strings, numbers, booleans and string lists).
pub fn json_to_toml(json: &Json) -> Option<toml::Value> {
    Some(match json {
        Json::Bool(b) => toml::Value::Boolean(*b),
        Json::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64()?),
        },
        Json::String(s) => toml::Value::String(s.clone()),
        Json::Array(items) => {
            toml::Value::Array(items.iter().map(json_to_toml).collect::<Option<Vec<_>>>()?)
        }
        _ => return None,
    })
}

/// A namespace the settings UI can show: its title and how to read its current and default
/// values as JSON.
#[derive(Clone)]
pub struct CatalogEntry {
    pub namespace: &'static str,
    pub workspace_overridable: bool,
    current: Rc<dyn Fn(&SettingsStore) -> Json>,
    pub default: Json,
}

impl std::fmt::Debug for CatalogEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CatalogEntry")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl CatalogEntry {
    fn of<T: Setting>() -> Self {
        Self {
            namespace: T::NAMESPACE,
            workspace_overridable: T::WORKSPACE_OVERRIDABLE,
            current: Rc::new(|store| {
                serde_json::to_value(&*store.get::<T>()).unwrap_or(Json::Null)
            }),
            default: serde_json::to_value(T::default()).unwrap_or(Json::Null),
        }
    }

    pub fn current(&self, store: &SettingsStore) -> Json {
        (self.current)(store)
    }
}

/// All namespaces of the app, in display order.
pub fn catalog() -> Vec<CatalogEntry> {
    vec![
        CatalogEntry::of::<AppearanceSettings>(),
        CatalogEntry::of::<AppSettings>(),
        CatalogEntry::of::<TerminalSettings>(),
        CatalogEntry::of::<LayoutSettings>(),
        CatalogEntry::of::<SessionsPanelSettings>(),
        CatalogEntry::of::<InspectorPanelSettings>(),
        CatalogEntry::of::<OverviewPanelSettings>(),
        CatalogEntry::of::<WorkspacesSettings>(),
        CatalogEntry::of::<HistorySettings>(),
        CatalogEntry::of::<ChatSettings>(),
        CatalogEntry::of::<AgentsSettings>(),
        CatalogEntry::of::<ResourcesSettings>(),
        CatalogEntry::of::<AssistantSettings>(),
        CatalogEntry {
            namespace: "accounts",
            workspace_overridable: false,
            current: Rc::new(|_| Json::Null),
            default: Json::Null,
        },
        CatalogEntry {
            namespace: "models",
            workspace_overridable: false,
            current: Rc::new(|_| Json::Null),
            default: Json::Null,
        },
    ]
}

/// The schema of `namespace` from `store.schema()`.
pub fn namespace_schema(schema: &Json, namespace: &str) -> Option<Json> {
    schema
        .get("namespaces")?
        .get(namespace)?
        .get("schema")
        .cloned()
}

/// Namespaces matching `query` (case-insensitive substring of the namespace or of any field
/// key or doc), in catalog order.
pub fn search<'a>(
    entries: &'a [CatalogEntry],
    schemas: &BTreeMap<&'static str, Vec<Field>>,
    query: &str,
) -> Vec<&'a CatalogEntry> {
    let query = query.trim().to_lowercase();
    entries
        .iter()
        .filter(|entry| {
            query.is_empty()
                || entry.namespace.to_lowercase().contains(&query)
                || schemas.get(entry.namespace).is_some_and(|fields| {
                    fields.iter().any(|f| {
                        f.dotted().to_lowercase().contains(&query)
                            || f.doc.to_lowercase().contains(&query)
                    })
                })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hutan_settings::SettingsPaths;

    fn store() -> (tempfile::TempDir, SettingsStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
        crate::settings::register_all(&store);
        (dir, store)
    }

    fn fields(store: &SettingsStore, namespace: &str) -> Vec<Field> {
        namespace_schema(&store.schema(), namespace)
            .map(|s| fields_of(&s))
            .unwrap_or_default()
    }

    fn kind(fields: &[Field], path: &str) -> FieldKind {
        fields
            .iter()
            .find(|f| f.dotted() == path)
            .unwrap_or_else(|| panic!("no field {path}: {fields:?}"))
            .kind
            .clone()
    }

    #[test]
    fn terminal_fields_cover_every_widget_kind() {
        let (_dir, store) = store();
        let f = fields(&store, "terminal");
        assert_eq!(kind(&f, "font_family"), FieldKind::String);
        assert_eq!(kind(&f, "font_size"), FieldKind::Number);
        assert_eq!(kind(&f, "copy_on_select"), FieldKind::Bool);
        assert_eq!(
            kind(&f, "option_as_meta"),
            FieldKind::Enum(
                ["none", "left", "right", "both"]
                    .map(str::to_owned)
                    .to_vec()
            )
        );
        assert_eq!(
            kind(&f, "cursor.style"),
            FieldKind::Enum(["block", "bar", "underline"].map(str::to_owned).to_vec())
        );
        assert_eq!(kind(&f, "cursor.blink"), FieldKind::Bool);
        assert!(
            f.iter()
                .find(|f| f.dotted() == "font_size")
                .unwrap()
                .doc
                .contains("points"),
            "docs come from the field comments"
        );
    }

    #[test]
    fn lists_enums_and_maps_are_recognised() {
        let (_dir, store) = store();
        assert_eq!(
            kind(&fields(&store, "panels.sessions"), "row"),
            FieldKind::StringList
        );
        assert_eq!(
            kind(&fields(&store, "panels.sessions"), "group_by"),
            FieldKind::Enum(["workspace", "state", "none"].map(str::to_owned).to_vec())
        );
        assert!(matches!(
            kind(&fields(&store, "layout"), "presets"),
            FieldKind::Unsupported(_)
        ));
        assert_eq!(
            kind(&fields(&store, "app"), "on_last_window_close"),
            FieldKind::Enum(["quit", "menu_bar"].map(str::to_owned).to_vec())
        );
        assert_eq!(
            kind(&fields(&store, "app"), "keep_running"),
            FieldKind::Bool
        );
        assert_eq!(
            kind(&fields(&store, "appearance"), "density"),
            FieldKind::Enum(vec!["compact".into(), "comfortable".into()])
        );
    }

    #[test]
    fn catalog_values_and_toml_conversion() {
        let (_dir, store) = store();
        let entries = catalog();
        let terminal = entries.iter().find(|e| e.namespace == "terminal").unwrap();
        let current = terminal.current(&store);
        let size = value_at(&current, &["font_size".to_owned()]).unwrap();
        assert_eq!(json_to_toml(size), Some(toml::Value::Float(13.0)));
        assert_eq!(
            json_to_toml(&serde_json::json!(["a", "b"])),
            Some(toml::Value::Array(vec!["a".into(), "b".into()]))
        );
        assert!(terminal.workspace_overridable);
        assert_eq!(terminal.default, current);
    }

    #[test]
    fn search_matches_namespaces_and_field_docs() {
        let (_dir, store) = store();
        let entries = catalog();
        let schemas: BTreeMap<&'static str, Vec<Field>> = entries
            .iter()
            .map(|e| (e.namespace, fields(&store, e.namespace)))
            .collect();
        let names = |q: &str| -> Vec<&'static str> {
            search(&entries, &schemas, q)
                .into_iter()
                .map(|e| e.namespace)
                .collect()
        };
        assert_eq!(names("panels").len(), 3);
        assert_eq!(names("font"), ["terminal"]);
        assert_eq!(names("density"), ["appearance"]);
        assert_eq!(names("on_last_window_close"), ["app"]);
        assert!(names("zzzz").is_empty());
    }
}
