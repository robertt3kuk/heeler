//! The settings sheet (⌘,): generated from the settings store's schemas.
//!
//! Left: a searchable list of namespaces. Right: the fields of the selected namespace, one
//! control per schema type (toggle, dropdown, number/text box, string-list editor) with "Reset"
//! to the default. Bottom: the store's diagnostics. Every change goes through
//! `SettingsStore::write_user`, which edits `settings.toml` in place and keeps its comments.

use crate::theme::DensityExt as _;
use std::collections::BTreeMap;

use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use heeler_settings::Severity;
use serde_json::Value as Json;

use super::actions::sheet as sheet_actions;
use super::modal::PromptSpec;
use super::settings_schema::{
    CatalogEntry, Field, FieldKind, catalog, fields_of, json_to_toml, namespace_schema, search,
    value_at,
};
use super::view::Workbench;

/// A text box being edited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Edit {
    /// Dotted field path inside the namespace.
    pub field: String,
    pub text: String,
    /// The text is a new list item rather than the field's whole value.
    pub list_add: bool,
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct SettingsSheet {
    namespace: &'static str,
    query: String,
    edit: Option<Edit>,
    dropdown: Option<String>,
    message: Option<String>,
    entries: Vec<CatalogEntry>,
    fields: BTreeMap<&'static str, Vec<Field>>,
}

impl SettingsSheet {
    pub fn namespace(&self) -> &str {
        self.namespace
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn edit(&self) -> Option<&Edit> {
        self.edit.as_ref()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// The fields of the selected namespace.
    pub fn fields(&self) -> &[Field] {
        self.fields.get(self.namespace).map_or(&[], Vec::as_slice)
    }
}

fn display_value(json: &Json) -> String {
    match json {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

impl Workbench {
    // ---- open / close -----------------------------------------------------------------------

    /// Opens the sheet at `namespace` (the first one when `None` or unknown).
    pub fn open_settings(
        &mut self,
        namespace: Option<&str>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = catalog();
        let schema = self.settings.schema();
        let fields: BTreeMap<&'static str, Vec<Field>> = entries
            .iter()
            .map(|e| {
                (
                    e.namespace,
                    namespace_schema(&schema, e.namespace)
                        .map(|s| fields_of(&s))
                        .unwrap_or_default(),
                )
            })
            .collect();
        let selected = entries
            .iter()
            .find(|e| Some(e.namespace) == namespace)
            .or_else(|| {
                self.sheet
                    .as_ref()
                    .and_then(|s| entries.iter().find(|e| e.namespace == s.namespace))
            })
            .or_else(|| entries.first())
            .map_or("appearance", |e| e.namespace);
        self.palette = None;
        self.context_menu = None;
        self.sheet = Some(SettingsSheet {
            namespace: selected,
            query: String::new(),
            edit: None,
            dropdown: None,
            message: None,
            entries,
            fields,
        });
        window.focus(&self.sheet_focus, cx);
        cx.notify();
    }

    pub fn settings_sheet(&self) -> Option<&SettingsSheet> {
        self.sheet.as_ref()
    }

    pub fn close_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.sheet.take().is_some() {
            self.focus_root(window, cx);
            cx.notify();
        }
    }

    /// Escape: cancels an edit or a dropdown first, then closes the sheet.
    pub(crate) fn sheet_dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(sheet) = &mut self.sheet else {
            return;
        };
        if sheet.edit.take().is_some() || sheet.dropdown.take().is_some() {
            cx.notify();
        } else {
            self.close_settings(window, cx);
        }
    }

    pub fn select_settings_namespace(&mut self, namespace: &str, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet
            && let Some(entry) = sheet.entries.iter().find(|e| e.namespace == namespace)
        {
            sheet.namespace = entry.namespace;
            sheet.edit = None;
            sheet.dropdown = None;
            sheet.message = None;
            cx.notify();
        }
    }

    pub fn set_settings_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if let Some(sheet) = &mut self.sheet {
            sheet.query = query.to_owned();
            if let Some(first) = search(&sheet.entries, &sheet.fields, query).first()
                && search(&sheet.entries, &sheet.fields, query)
                    .iter()
                    .all(|e| e.namespace != sheet.namespace)
            {
                sheet.namespace = first.namespace;
            }
            cx.notify();
        }
    }

    // ---- reading ----------------------------------------------------------------------------

    fn sheet_field(&self, field: &str) -> Option<(&'static str, Field, Json)> {
        let sheet = self.sheet.as_ref()?;
        let entry = sheet
            .entries
            .iter()
            .find(|e| e.namespace == sheet.namespace)?;
        let f = sheet.fields().iter().find(|f| f.dotted() == field)?.clone();
        let current = entry.current(&self.settings);
        let value = value_at(&current, &f.path).cloned().unwrap_or(Json::Null);
        Some((entry.namespace, f, value))
    }

    /// The current value of `field` in the selected namespace, as JSON.
    pub fn sheet_value(&self, field: &str) -> Option<Json> {
        self.sheet_field(field).map(|(_, _, value)| value)
    }

    // ---- writing ----------------------------------------------------------------------------

    /// Writes `value` to `field` of the selected namespace in `settings.toml`.
    pub fn sheet_write(&mut self, field: &str, value: toml::Value, cx: &mut Context<Self>) {
        let Some(namespace) = self.sheet.as_ref().map(|s| s.namespace) else {
            return;
        };
        let key = format!("{namespace}.{field}");
        let message = match self.settings.write_user(&key, value) {
            Ok(()) => None,
            Err(err) => Some(err.to_string()),
        };
        if let Some(sheet) = &mut self.sheet {
            sheet.message = message;
            sheet.dropdown = None;
        }
        cx.notify();
    }

    pub fn sheet_toggle(&mut self, field: &str, cx: &mut Context<Self>) {
        if let Some((_, f, value)) = self.sheet_field(field)
            && f.kind == FieldKind::Bool
        {
            let on = value.as_bool().unwrap_or(false);
            self.sheet_write(field, toml::Value::Boolean(!on), cx);
        }
    }

    pub fn sheet_choose(&mut self, field: &str, choice: &str, cx: &mut Context<Self>) {
        if let Some((_, f, _)) = self.sheet_field(field)
            && matches!(&f.kind, FieldKind::Enum(values) if values.iter().any(|v| v == choice))
        {
            self.sheet_write(field, toml::Value::String(choice.to_owned()), cx);
        }
    }

    /// Restores the default value of `field`.
    pub fn sheet_reset(&mut self, field: &str, cx: &mut Context<Self>) {
        let Some(sheet) = &self.sheet else { return };
        let Some(entry) = sheet
            .entries
            .iter()
            .find(|e| e.namespace == sheet.namespace)
        else {
            return;
        };
        let Some(f) = sheet.fields().iter().find(|f| f.dotted() == field) else {
            return;
        };
        if let Some(default) = value_at(&entry.default, &f.path).and_then(json_to_toml) {
            self.sheet_write(field, default, cx);
        }
    }

    pub fn sheet_remove_list_item(&mut self, field: &str, index: usize, cx: &mut Context<Self>) {
        let Some((_, f, value)) = self.sheet_field(field) else {
            return;
        };
        if f.kind != FieldKind::StringList {
            return;
        }
        let mut items: Vec<String> = value
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        if index < items.len() {
            items.remove(index);
            self.sheet_write(
                field,
                toml::Value::Array(items.into_iter().map(toml::Value::String).collect()),
                cx,
            );
        }
    }

    /// Moves item `index` of a string list by `delta` places (negative = up), keeping the
    /// order the user wants (e.g. the fields of `panels.sessions.row`).
    pub fn sheet_move_list_item(
        &mut self,
        field: &str,
        index: usize,
        delta: isize,
        cx: &mut Context<Self>,
    ) {
        let Some((_, f, value)) = self.sheet_field(field) else {
            return;
        };
        if f.kind != FieldKind::StringList {
            return;
        }
        let mut items: Vec<String> = value
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let Some(target) = index
            .checked_add_signed(delta)
            .filter(|t| *t < items.len() && index < items.len())
        else {
            return;
        };
        items.swap(index, target);
        self.sheet_write(
            field,
            toml::Value::Array(items.into_iter().map(toml::Value::String).collect()),
            cx,
        );
    }

    // ---- text editing -----------------------------------------------------------------------

    /// Starts editing `field` (a number/string box, or "add item" of a list).
    pub fn sheet_begin_edit(&mut self, field: &str, cx: &mut Context<Self>) {
        let Some((_, f, value)) = self.sheet_field(field) else {
            return;
        };
        let (text, list_add) = match f.kind {
            FieldKind::Integer | FieldKind::Number | FieldKind::String => {
                (display_value(&value), false)
            }
            FieldKind::StringList => (String::new(), true),
            _ => return,
        };
        if let Some(sheet) = &mut self.sheet {
            sheet.dropdown = None;
            sheet.edit = Some(Edit {
                field: field.to_owned(),
                text,
                list_add,
                error: None,
            });
            cx.notify();
        }
    }

    /// Replaces the text of the edit in progress.
    pub fn sheet_set_edit_text(&mut self, text: &str, cx: &mut Context<Self>) {
        if let Some(Edit {
            text: current,
            error,
            ..
        }) = self.sheet.as_mut().and_then(|s| s.edit.as_mut())
        {
            *current = text.to_owned();
            *error = None;
            cx.notify();
        }
    }

    /// Validates and writes the edit in progress (Enter). A value that does not fit the field
    /// stays in the box with an error.
    pub fn sheet_commit_edit(&mut self, cx: &mut Context<Self>) {
        let Some(edit) = self.sheet.as_ref().and_then(|s| s.edit.clone()) else {
            return;
        };
        let Some((_, f, value)) = self.sheet_field(&edit.field) else {
            return;
        };
        let text = edit.text.trim();
        let parsed: Result<toml::Value, String> = match (&f.kind, edit.list_add) {
            (FieldKind::Integer, _) => text
                .parse::<i64>()
                .map(toml::Value::Integer)
                .map_err(|_| "expected a whole number".to_owned()),
            (FieldKind::Number, _) => text
                .parse::<f64>()
                .ok()
                .filter(|n| n.is_finite())
                .map(toml::Value::Float)
                .ok_or_else(|| "expected a number".to_owned()),
            (FieldKind::String, _) => Ok(toml::Value::String(edit.text.clone())),
            (FieldKind::StringList, true) => {
                if text.is_empty() {
                    Err("type an item first".to_owned())
                } else {
                    let mut items: Vec<toml::Value> = value
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| {
                                    v.as_str().map(|s| toml::Value::String(s.to_owned()))
                                })
                                .collect()
                        })
                        .unwrap_or_default();
                    items.push(toml::Value::String(text.to_owned()));
                    Ok(toml::Value::Array(items))
                }
            }
            _ => Err("this field cannot be edited here".to_owned()),
        };
        match parsed {
            Ok(value) => {
                if let Some(sheet) = &mut self.sheet {
                    sheet.edit = None;
                }
                self.sheet_write(&edit.field, value, cx);
            }
            Err(message) => {
                if let Some(Edit { error, .. }) = self.sheet.as_mut().and_then(|s| s.edit.as_mut())
                {
                    *error = Some(message);
                }
                cx.notify();
            }
        }
    }

    fn sheet_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let ks = &event.keystroke;
        if ks.modifiers.platform || ks.modifiers.control || ks.modifiers.function {
            return;
        }
        let Some(sheet) = &mut self.sheet else {
            return;
        };
        if ks.key == "enter" {
            if sheet.edit.is_some() {
                self.sheet_commit_edit(cx);
            }
            return;
        }
        let was_query = sheet.edit.is_none();
        let target: &mut String = match &mut sheet.edit {
            Some(edit) => {
                edit.error = None;
                &mut edit.text
            }
            None => &mut sheet.query,
        };
        if ks.key == "backspace" {
            target.pop();
        } else if let Some(text) = &ks.key_char {
            target.push_str(text);
        } else {
            return;
        }
        if was_query {
            let query = sheet.query.clone();
            self.set_settings_query(&query, cx);
        }
        cx.notify();
    }

    // ---- save layout as preset --------------------------------------------------------------

    /// Prompts for a name and writes the current docks and status bar as `layout.presets.<name>`.
    pub fn save_layout_as_preset(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open_prompt(
            PromptSpec {
                title: "Save layout as preset",
                message: "Preset name (letters, digits, - and _):",
                initial: "",
                confirm_label: "Save",
            },
            |this, name, _window, cx| this.write_layout_preset(&name, cx),
            window,
            cx,
        );
    }

    /// Writes the current layout as the user preset `name` and makes it the current preset.
    pub fn write_layout_preset(&mut self, name: &str, cx: &mut Context<Self>) {
        let name: String = name
            .trim()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '-'
                }
            })
            .collect();
        if name.is_empty() {
            self.set_notice("A preset needs a name", cx);
            return;
        }
        let preset =
            crate::settings::LayoutSettings::preset_from(&self.state.docks, &self.status_bar());
        let value = match toml::Value::try_from(preset) {
            Ok(value) => value,
            Err(err) => {
                self.set_notice(format!("Cannot save the preset: {err}"), cx);
                return;
            }
        };
        match self
            .settings
            .write_user(&format!("layout.presets.{name}"), value)
        {
            Ok(()) => {
                // The preset is built from what is on screen: it is the current one now.
                self.state.preset = name.clone();
                self.set_notice(format!("Saved layout preset “{name}”"), cx);
            }
            Err(err) => self.set_notice(format!("Cannot save the preset: {err}"), cx),
        }
    }

    // ---- rendering --------------------------------------------------------------------------

    pub(crate) fn render_settings_sheet(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let sheet = self.sheet.as_ref()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.hover;
        let accent = theme.accent;
        let selected = sheet.namespace;
        let editing = sheet.edit.clone();
        let dropdown = sheet.dropdown.clone();
        let message = sheet.message.clone();
        let query = sheet.query.clone();
        let namespaces: Vec<&'static str> = search(&sheet.entries, &sheet.fields, &query)
            .into_iter()
            .map(|e| e.namespace)
            .collect();
        let entry = sheet.entries.iter().find(|e| e.namespace == selected)?;
        let overridable = entry.workspace_overridable;
        let current = entry.current(&self.settings);
        let default = entry.default.clone();
        let fields: Vec<Field> = sheet.fields().to_vec();
        let errors = self.settings.errors();

        let nav: Vec<AnyElement> = namespaces
            .iter()
            .map(|ns| {
                let ns: &'static str = ns;
                div()
                    .id(SharedString::from(format!("ns-{ns}")))
                    .flex()
                    .items_center()
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 3.0))
                    .rounded(px(tokens.radius - 1.0))
                    .cursor_pointer()
                    .when(ns == selected, move |s| s.bg(accent.opacity(0.22)))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_settings_namespace(ns, cx);
                    }))
                    .child(SharedString::from(ns))
                    .into_any_element()
            })
            .collect();

        let button = |id: SharedString, label: SharedString, active: bool| {
            div()
                .id(id)
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.sidebar_row - 4.0))
                .flex()
                .items_center()
                .rounded(px(tokens.radius - 2.0))
                .border_1()
                .border_color(if active { accent } else { theme.border })
                .when(active, move |s| s.bg(accent.opacity(0.2)))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .child(label)
        };

        let rows: Vec<AnyElement> = fields
            .iter()
            .map(|field| {
                let key = field.dotted();
                let value = value_at(&current, &field.path)
                    .cloned()
                    .unwrap_or(Json::Null);
                let is_default = value_at(&default, &field.path) == Some(&value);
                let control: AnyElement = match &field.kind {
                    FieldKind::Bool => {
                        let on = value.as_bool().unwrap_or(false);
                        let k = key.clone();
                        button(
                            SharedString::from(format!("toggle-{key}")),
                            if on { "On".into() } else { "Off".into() },
                            on,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| this.sheet_toggle(&k, cx)))
                        .into_any_element()
                    }
                    FieldKind::Enum(options) => {
                        let open = dropdown.as_deref() == Some(key.as_str());
                        let k = key.clone();
                        let head = button(
                            SharedString::from(format!("dropdown-{key}")),
                            SharedString::from(format!("{} ▾", display_value(&value))),
                            open,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(sheet) = &mut this.sheet {
                                sheet.dropdown = if sheet.dropdown.as_deref() == Some(k.as_str()) {
                                    None
                                } else {
                                    Some(k.clone())
                                };
                                cx.notify();
                            }
                        }));
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap(px(tokens.spacing))
                            .child(head)
                            .when(open, |s| {
                                s.children(options.iter().map(|option| {
                                    let (k, o) = (key.clone(), option.clone());
                                    button(
                                        SharedString::from(format!("option-{key}-{option}")),
                                        SharedString::from(option.clone()),
                                        Json::String(option.clone()) == value,
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.sheet_choose(&k, &o, cx);
                                        },
                                    ))
                                }))
                            })
                            .into_any_element()
                    }
                    FieldKind::Integer | FieldKind::Number | FieldKind::String => {
                        let k = key.clone();
                        let active = editing.as_ref().filter(|e| e.field == key);
                        let text = active.map_or_else(|| display_value(&value), |e| e.text.clone());
                        let error = active.and_then(|e| e.error.clone());
                        div()
                            .flex()
                            .flex_col()
                            .items_end()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .id(SharedString::from(format!("input-{key}")))
                                    .flex()
                                    .items_center()
                                    .min_w(px(150.0))
                                    .h(px(tokens.sidebar_row - 2.0))
                                    .px(px(tokens.spacing * 2.0))
                                    .rounded(px(tokens.radius - 2.0))
                                    .border_1()
                                    .border_color(if error.is_some() {
                                        theme.error
                                    } else if active.is_some() {
                                        accent
                                    } else {
                                        theme.border
                                    })
                                    .cursor_text()
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.sheet_begin_edit(&k, cx);
                                    }))
                                    .child(SharedString::from(text))
                                    .when(active.is_some(), |s| {
                                        s.child(div().w(px(1.0)).h(px(tokens.font_size)).bg(accent))
                                    }),
                            )
                            .when_some(error, |s, error| {
                                s.child(
                                    div()
                                        .text_size(px(tokens.section_font_size))
                                        .text_color(theme.error)
                                        .child(SharedString::from(error)),
                                )
                            })
                            .into_any_element()
                    }
                    FieldKind::StringList => {
                        let items: Vec<String> = value
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .filter_map(|v| v.as_str().map(str::to_owned))
                                    .collect()
                            })
                            .unwrap_or_default();
                        let adding = editing.as_ref().filter(|e| e.field == key);
                        let k = key.clone();
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap(px(tokens.spacing))
                            .max_w(px(320.0))
                            .children(items.iter().enumerate().map(|(index, item)| {
                                let k = key.clone();
                                let (k_up, k_down) = (key.clone(), key.clone());
                                let last = items.len() - 1;
                                let step = |id: String, label: &'static str, enabled: bool| {
                                    div()
                                        .id(SharedString::from(id))
                                        .text_color(if enabled { theme.fg } else { theme.border })
                                        .when(enabled, |s| s.cursor_pointer())
                                        .child(label)
                                };
                                div()
                                    .id(SharedString::from(format!("item-{key}-{index}")))
                                    .flex()
                                    .items_center()
                                    .gap(px(tokens.spacing))
                                    .px(px(tokens.spacing * 2.0))
                                    .h(px(tokens.sidebar_row - 4.0))
                                    .rounded(px(tokens.radius - 2.0))
                                    .border_1()
                                    .border_color(theme.border)
                                    .child(SharedString::from(item.clone()))
                                    .child(
                                        step(format!("up-{key}-{index}"), "↑", index > 0).on_click(
                                            cx.listener(move |this, _, _, cx| {
                                                this.sheet_move_list_item(&k_up, index, -1, cx);
                                            }),
                                        ),
                                    )
                                    .child(
                                        step(format!("down-{key}-{index}"), "↓", index < last)
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.sheet_move_list_item(&k_down, index, 1, cx);
                                            })),
                                    )
                                    .child(
                                        div()
                                            .id(SharedString::from(format!("remove-{key}-{index}")))
                                            .text_color(theme.muted)
                                            .cursor_pointer()
                                            .on_click(cx.listener(move |this, _, _, cx| {
                                                this.sheet_remove_list_item(&k, index, cx);
                                            }))
                                            .child("×"),
                                    )
                            }))
                            .child(match adding {
                                Some(edit) => div()
                                    .flex()
                                    .items_center()
                                    .px(px(tokens.spacing * 2.0))
                                    .h(px(tokens.sidebar_row - 4.0))
                                    .rounded(px(tokens.radius - 2.0))
                                    .border_1()
                                    .border_color(if edit.error.is_some() {
                                        theme.error
                                    } else {
                                        accent
                                    })
                                    .min_w(px(80.0))
                                    .child(SharedString::from(edit.text.clone()))
                                    .child(div().w(px(1.0)).h(px(tokens.font_size)).bg(accent))
                                    .into_any_element(),
                                None => button(
                                    SharedString::from(format!("add-{key}")),
                                    "+ add".into(),
                                    false,
                                )
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.sheet_begin_edit(&k, cx);
                                }))
                                .into_any_element(),
                            })
                            .into_any_element()
                    }
                    FieldKind::Unsupported(hint) => div()
                        .text_color(theme.muted)
                        .max_w(px(260.0))
                        .child(SharedString::from(format!("{} — {hint}", value)))
                        .into_any_element(),
                };
                let reset =
                    (!is_default && !matches!(field.kind, FieldKind::Unsupported(_))).then(|| {
                        let k = key.clone();
                        button(
                            SharedString::from(format!("reset-{key}")),
                            "Reset".into(),
                            false,
                        )
                        .text_color(theme.muted)
                        .on_click(cx.listener(move |this, _, _, cx| this.sheet_reset(&k, cx)))
                    });
                div()
                    .id(SharedString::from(format!("field-{key}")))
                    .flex()
                    .items_start()
                    .justify_between()
                    .gap(px(tokens.spacing * 3.0))
                    .py(px(tokens.spacing * 2.0))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(SharedString::from(key.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(tokens.section_font_size + 1.0))
                                    .text_color(theme.muted)
                                    .child(SharedString::from(field.doc.clone())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_start()
                            .gap(px(tokens.spacing * 2.0))
                            .children(reset)
                            .child(control),
                    )
                    .into_any_element()
            })
            .collect();

        let problems: Vec<AnyElement> = errors
            .iter()
            .map(|e| {
                let color = if e.is_error() {
                    theme.error
                } else {
                    theme.warning
                };
                div()
                    .flex()
                    .gap(px(tokens.spacing * 2.0))
                    .text_size(px(tokens.section_font_size + 1.0))
                    .child(
                        div()
                            .flex_none()
                            .text_color(color)
                            .child(SharedString::from(e.severity.to_string())),
                    )
                    .child(div().min_w_0().child(SharedString::from(
                        match (&e.file, e.key_path.is_empty()) {
                            (Some(file), false) => {
                                format!("{} · {}: {}", file.display(), e.key_path, e.message)
                            }
                            (Some(file), true) => format!("{}: {}", file.display(), e.message),
                            (None, false) => format!("{}: {}", e.key_path, e.message),
                            (None, true) => e.message.clone(),
                        },
                    )))
                    .into_any_element()
            })
            .collect();
        let has_errors = errors.iter().any(|e| e.severity == Severity::Error);

        let search_box = div()
            .id("sheet-search")
            .flex()
            .items_center()
            .h(px(tokens.sidebar_row))
            .mx(px(tokens.spacing * 2.0))
            .mb(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius - 1.0))
            .border_1()
            .border_color(if editing.is_none() {
                accent
            } else {
                theme.border
            })
            .on_click(cx.listener(|this, _, _, cx| {
                if let Some(sheet) = &mut this.sheet {
                    sheet.edit = None;
                    cx.notify();
                }
            }))
            .child(if query.is_empty() {
                div().text_color(theme.muted).child("⌕ Search settings")
            } else {
                div().child(SharedString::from(query.clone()))
            });

        Some(
            div()
                .id("sheet-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(48.0))
                .bg(gpui::black().opacity(0.3))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_settings(window, cx)),
                )
                .child(
                    div()
                        .id("settings-sheet")
                        .key_context("SettingsSheet")
                        .track_focus(&self.sheet_focus)
                        .w(px(860.0))
                        .h(px(600.0))
                        .flex()
                        .flex_col()
                        .rounded(px(tokens.radius + 4.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .overflow_hidden()
                        .text_size(px(tokens.font_size))
                        .text_color(theme.fg)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            this.sheet_key(event, cx);
                        }))
                        .on_action(cx.listener(|this, _: &sheet_actions::Dismiss, window, cx| {
                            this.sheet_dismiss(window, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_none()
                                        .w(px(220.0))
                                        .py(px(tokens.spacing * 3.0))
                                        .border_r_1()
                                        .border_color(theme.border)
                                        .child(search_box)
                                        .child(
                                            div()
                                                .id("sheet-nav")
                                                .flex_1()
                                                .overflow_y_scroll()
                                                .px(px(tokens.spacing))
                                                .children(nav),
                                        ),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_between()
                                                .h(px(tokens.sidebar_row + 8.0))
                                                .px(px(tokens.spacing * 4.0))
                                                .border_b_1()
                                                .border_color(theme.border)
                                                .child(
                                                    div()
                                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                                        .child(SharedString::from(selected)),
                                                )
                                                .child(
                                                    div()
                                                        .text_size(px(tokens.section_font_size))
                                                        .text_color(theme.muted)
                                                        .child(if overridable {
                                                            "workspace-overridable"
                                                        } else {
                                                            ""
                                                        }),
                                                ),
                                        )
                                        .child(
                                            div()
                                                .id("sheet-fields")
                                                .flex_1()
                                                .min_h_0()
                                                .overflow_y_scroll()
                                                .px(px(tokens.spacing * 4.0))
                                                .when(rows.is_empty(), |s| {
                                                    s.child(
                                                        div()
                                                            .py(px(tokens.spacing * 3.0))
                                                            .text_color(theme.muted)
                                                            .child("No editable settings."),
                                                    )
                                                })
                                                .children(rows),
                                        )
                                        .when_some(message, |s, message| {
                                            s.child(
                                                div()
                                                    .px(px(tokens.spacing * 4.0))
                                                    .py(px(tokens.spacing))
                                                    .text_color(theme.error)
                                                    .child(SharedString::from(message)),
                                            )
                                        }),
                                ),
                        )
                        .child(
                            div()
                                .id("sheet-errors")
                                .flex()
                                .flex_col()
                                .flex_none()
                                .max_h(px(110.0))
                                .overflow_y_scroll()
                                .gap(px(2.0))
                                .px(px(tokens.spacing * 4.0))
                                .py(px(tokens.spacing * 2.0))
                                .border_t_1()
                                .border_color(theme.border)
                                .child(
                                    div()
                                        .text_size(px(tokens.section_font_size))
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .text_color(if has_errors {
                                            theme.error
                                        } else {
                                            theme.muted
                                        })
                                        .child(if errors.is_empty() {
                                            "PROBLEMS · none"
                                        } else {
                                            "PROBLEMS"
                                        }),
                                )
                                .children(problems),
                        ),
                )
                .into_any_element(),
        )
    }
}
