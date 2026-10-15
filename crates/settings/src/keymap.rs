//! `keymap.toml`: key syntax, bindings, merging with caller-supplied defaults.
//!
//! ```toml
//! [[binding]]
//! keys = "cmd-shift-d"            # sequences are space separated: "cmd-k cmd-s"
//! action = "workbench::SplitDown" # "none" unbinds a default
//! context = "Pane"                # optional; omitted = global
//! ```
//!
//! The output is GPUI-free data ([`BindingSpec`]); the desktop converts it into GPUI bindings.
//! Defaults come first in the result and user bindings after them, so later entries win.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use crate::error::{SettingsError, syntax_error};

/// Action name that unbinds default bindings instead of binding something.
pub const UNBIND: &str = "none";

/// Why a key string is not valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum KeystrokeError {
    #[error("empty key string")]
    Empty,
    #[error("duplicate modifier `{0}`")]
    DuplicateModifier(&'static str),
    #[error("`{0}` is only a modifier; a key is required")]
    MissingKey(String),
    #[error("uppercase key `{0}`; write shift-{lower} instead", lower = .0.to_lowercase())]
    UppercaseKey(String),
    #[error("unknown key `{0}`")]
    UnknownKey(String),
}

/// One chord: modifiers plus a key. `key` is lowercase: a single character (`d`, `,`, `-`) or a
/// canonical name (`enter`, `escape`, `tab`, `space`, `backspace`, `delete`, `up`, `down`, `left`,
/// `right`, `home`, `end`, `pageup`, `pagedown`, `insert`, `f1`…`f24`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Keystroke {
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
    pub cmd: bool,
    pub function: bool,
    pub key: String,
}

impl fmt::Display for Keystroke {
    /// Canonical form, e.g. `ctrl-alt-shift-cmd-fn-d`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (on, name) in [
            (self.ctrl, "ctrl"),
            (self.alt, "alt"),
            (self.shift, "shift"),
            (self.cmd, "cmd"),
            (self.function, "fn"),
        ] {
            if on {
                write!(f, "{name}-")?;
            }
        }
        f.write_str(&self.key)
    }
}

const NAMED_KEYS: &[&str] = &[
    "enter",
    "escape",
    "tab",
    "space",
    "backspace",
    "delete",
    "up",
    "down",
    "left",
    "right",
    "home",
    "end",
    "pageup",
    "pagedown",
    "insert",
];

fn canonical_key(raw: &str) -> Result<String, KeystrokeError> {
    let mut chars = raw.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        if c.is_uppercase() {
            return Err(KeystrokeError::UppercaseKey(raw.to_owned()));
        }
        if c.is_control() || c.is_whitespace() {
            return Err(KeystrokeError::UnknownKey(raw.escape_default().to_string()));
        }
        return Ok(raw.to_owned());
    }
    let lower = raw.to_lowercase();
    let name = match lower.as_str() {
        "esc" => "escape",
        "return" => "enter",
        "pgup" => "pageup",
        "pgdn" => "pagedown",
        other => other,
    };
    if NAMED_KEYS.contains(&name) {
        return Ok(name.to_owned());
    }
    if let Some(n) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok())
        && (1..=24).contains(&n)
        && name == format!("f{n}")
    {
        return Ok(name.to_owned());
    }
    if modifier(&lower).is_some() {
        return Err(KeystrokeError::MissingKey(raw.to_owned()));
    }
    Err(KeystrokeError::UnknownKey(raw.to_owned()))
}

/// Modifier spelled `word` (case-insensitive) -> canonical name.
fn modifier(word: &str) -> Option<&'static str> {
    Some(match word.to_ascii_lowercase().as_str() {
        "cmd" | "command" => "cmd",
        "ctrl" | "control" => "ctrl",
        "alt" | "option" | "opt" => "alt",
        "shift" => "shift",
        "fn" => "fn",
        _ => return None,
    })
}

impl FromStr for Keystroke {
    type Err = KeystrokeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.is_empty() {
            return Err(KeystrokeError::Empty);
        }
        let mut stroke = Keystroke {
            ctrl: false,
            alt: false,
            shift: false,
            cmd: false,
            function: false,
            key: String::new(),
        };
        let mut rest = s;
        // A trailing `-` is the key itself (`cmd--`), so a modifier needs a non-empty tail.
        while let Some((head, tail)) = rest.split_once('-')
            && !tail.is_empty()
            && let Some(name) = modifier(head)
        {
            let slot = match name {
                "ctrl" => &mut stroke.ctrl,
                "alt" => &mut stroke.alt,
                "shift" => &mut stroke.shift,
                "cmd" => &mut stroke.cmd,
                _ => &mut stroke.function,
            };
            if std::mem::replace(slot, true) {
                return Err(KeystrokeError::DuplicateModifier(name));
            }
            rest = tail;
        }
        stroke.key = canonical_key(rest)?;
        Ok(stroke)
    }
}

/// Parses a whitespace-separated key sequence (`cmd-k cmd-s`).
pub fn parse_keys(s: &str) -> Result<Vec<Keystroke>, KeystrokeError> {
    let keys = s
        .split_whitespace()
        .map(str::parse)
        .collect::<Result<Vec<Keystroke>, _>>()?;
    if keys.is_empty() {
        return Err(KeystrokeError::Empty);
    }
    Ok(keys)
}

/// Formats a key sequence the way [`parse_keys`] reads it.
pub fn format_keys(keys: &[Keystroke]) -> String {
    keys.iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

/// One resolved binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingSpec {
    pub keys: Vec<Keystroke>,
    /// Namespaced action name such as `workbench::SplitDown`.
    pub action: String,
    /// GPUI key context (`Pane`, `Terminal`, …); `None` = global.
    pub context: Option<String>,
}

impl BindingSpec {
    /// Convenience for building default bindings: `BindingSpec::new("cmd-d", "workbench::SplitRight", None)`.
    pub fn new(keys: &str, action: &str, context: Option<&str>) -> Result<Self, KeystrokeError> {
        Ok(Self {
            keys: parse_keys(keys)?,
            action: action.to_owned(),
            context: context.map(str::to_owned),
        })
    }
}

/// Merged keymap plus everything worth telling the user.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct KeymapLoad {
    /// Defaults (minus unbound/overridden ones) followed by user bindings; later entries win.
    pub bindings: Vec<BindingSpec>,
    /// Invalid bindings (errors, skipped), conflicts and unknown keys (warnings).
    pub diagnostics: Vec<SettingsError>,
}

fn valid_action(action: &str) -> bool {
    let ident = |s: &str| {
        let mut chars = s.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    action == UNBIND || action.split("::").all(ident)
}

/// Loads `path` and merges it onto `defaults`. A missing file yields the defaults. `Err` only for
/// unreadable files or invalid TOML; bad individual bindings become diagnostics and are skipped.
pub fn load(path: &Path, defaults: &[BindingSpec]) -> Result<KeymapLoad, SettingsError> {
    match std::fs::read_to_string(path) {
        Ok(text) => load_str(defaults, &text, Some(path)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => load_str(defaults, "", Some(path)),
        Err(e) => Err(SettingsError::error(
            Some(path),
            "",
            format!("cannot read file: {e}"),
        )),
    }
}

/// Like [`load`] with the file contents already in hand. `file` is only used in diagnostics.
pub fn load_str(
    defaults: &[BindingSpec],
    text: &str,
    file: Option<&Path>,
) -> Result<KeymapLoad, SettingsError> {
    let mut diagnostics = Vec::new();
    let user = parse_user(text, file, &mut diagnostics)?;

    let defaults = dedup("keymap defaults", None, defaults.to_vec(), &mut diagnostics);
    let user = dedup("", file, user, &mut diagnostics);

    let mut bindings: Vec<BindingSpec> = defaults
        .into_iter()
        .filter(|d| {
            !user.iter().any(|u| {
                u.keys == d.keys
                    && (u.context == d.context || (u.action == UNBIND && u.context.is_none()))
            })
        })
        .collect();
    bindings.extend(user.into_iter().filter(|u| u.action != UNBIND));
    shadowed_prefixes(&bindings, file, &mut diagnostics);
    Ok(KeymapLoad {
        bindings,
        diagnostics,
    })
}

/// Parses the user's file into bindings (including `none` ones), reporting invalid entries.
fn parse_user(
    text: &str,
    file: Option<&Path>,
    diagnostics: &mut Vec<SettingsError>,
) -> Result<Vec<BindingSpec>, SettingsError> {
    let table: toml::Table = toml::from_str(text).map_err(|e| match file {
        Some(file) => syntax_error(file, text, &e),
        None => SettingsError::error(None, "", format!("invalid TOML: {}", e.message())),
    })?;
    for key in table.keys().filter(|k| *k != "binding") {
        diagnostics.push(SettingsError::warning(
            file,
            key.clone(),
            "unknown key (expected `[[binding]]` tables)",
        ));
    }
    let Some(entries) = table.get("binding") else {
        return Ok(Vec::new());
    };
    let Some(entries) = entries.as_array() else {
        return Err(SettingsError::error(
            file,
            "binding",
            "expected an array of tables (`[[binding]]`)",
        ));
    };

    let mut out = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let at = |field: &str| format!("binding[{index}].{field}");
        let Some(entry) = entry.as_table() else {
            diagnostics.push(SettingsError::error(
                file,
                format!("binding[{index}]"),
                "expected a table",
            ));
            continue;
        };
        for key in entry
            .keys()
            .filter(|k| !matches!(k.as_str(), "keys" | "action" | "context"))
        {
            diagnostics.push(SettingsError::warning(file, at(key), "unknown key"));
        }
        let field = |name: &str| entry.get(name).map(|v| v.as_str());
        let keys = match field("keys") {
            Some(Some(keys)) => match parse_keys(keys) {
                Ok(keys) => keys,
                Err(e) => {
                    diagnostics.push(SettingsError::error(
                        file,
                        at("keys"),
                        format!("invalid keys `{keys}`: {e}"),
                    ));
                    continue;
                }
            },
            Some(None) => {
                diagnostics.push(SettingsError::error(file, at("keys"), "expected a string"));
                continue;
            }
            None => {
                diagnostics.push(SettingsError::error(file, at("keys"), "missing `keys`"));
                continue;
            }
        };
        let action = match field("action") {
            Some(Some(action)) if valid_action(action) => action.to_owned(),
            Some(Some(action)) => {
                diagnostics.push(SettingsError::error(
                    file,
                    at("action"),
                    format!("invalid action `{action}`; expected `namespace::Name` or `{UNBIND}`"),
                ));
                continue;
            }
            Some(None) => {
                diagnostics.push(SettingsError::error(
                    file,
                    at("action"),
                    "expected a string",
                ));
                continue;
            }
            None => {
                diagnostics.push(SettingsError::error(file, at("action"), "missing `action`"));
                continue;
            }
        };
        let context = match field("context") {
            None => None,
            Some(Some(context)) if !context.trim().is_empty() => Some(context.to_owned()),
            Some(_) => {
                diagnostics.push(SettingsError::error(
                    file,
                    at("context"),
                    "expected a non-empty string",
                ));
                continue;
            }
        };
        out.push(BindingSpec {
            keys,
            action,
            context,
        });
    }
    Ok(out)
}

/// Same keys + same context twice in one source: warn, last one wins.
fn dedup(
    source: &str,
    file: Option<&Path>,
    bindings: Vec<BindingSpec>,
    diagnostics: &mut Vec<SettingsError>,
) -> Vec<BindingSpec> {
    let mut out: Vec<BindingSpec> = Vec::with_capacity(bindings.len());
    for binding in bindings {
        if let Some(pos) = out
            .iter()
            .position(|b| b.keys == binding.keys && b.context == binding.context)
        {
            let earlier = out.remove(pos);
            let source = if source.is_empty() {
                String::new()
            } else {
                format!("{source}: ")
            };
            diagnostics.push(SettingsError::warning(
                file,
                "binding",
                format!(
                    "{source}conflict: `{}`{} is bound to both `{}` and `{}`; the last one wins",
                    format_keys(&binding.keys),
                    context_suffix(&binding.context),
                    earlier.action,
                    binding.action,
                ),
            ));
        }
        out.push(binding);
    }
    out
}

fn context_suffix(context: &Option<String>) -> String {
    context
        .as_ref()
        .map(|c| format!(" in context `{c}`"))
        .unwrap_or_default()
}

/// A binding whose keys are a strict prefix of another binding's (same context) makes the longer
/// sequence unreachable or ambiguous.
fn shadowed_prefixes(
    bindings: &[BindingSpec],
    file: Option<&Path>,
    diagnostics: &mut Vec<SettingsError>,
) {
    for short in bindings {
        for long in bindings {
            if short.context == long.context
                && short.keys.len() < long.keys.len()
                && long.keys.starts_with(&short.keys)
            {
                diagnostics.push(SettingsError::warning(
                    file,
                    "binding",
                    format!(
                        "conflict: `{}`{} ({}) is a prefix of `{}` ({})",
                        format_keys(&short.keys),
                        context_suffix(&short.context),
                        short.action,
                        format_keys(&long.keys),
                        long.action,
                    ),
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(keys: &str, action: &str, context: Option<&str>) -> BindingSpec {
        BindingSpec::new(keys, action, context).unwrap()
    }

    fn defaults() -> Vec<BindingSpec> {
        vec![
            spec("cmd-d", "workbench::SplitRight", Some("Pane")),
            spec("cmd-shift-d", "workbench::SplitDown", Some("Pane")),
            spec("cmd-k", "palette::Toggle", None),
        ]
    }

    #[test]
    fn parses_modifiers_and_keys() {
        let k: Keystroke = "cmd-shift-d".parse().unwrap();
        assert!(k.cmd && k.shift && !k.ctrl && !k.alt && !k.function);
        assert_eq!(k.key, "d");

        let k: Keystroke = "Option-Ctrl-fn-F12".parse().unwrap();
        assert!(k.alt && k.ctrl && k.function);
        assert_eq!(k.key, "f12");
        assert_eq!(k.to_string(), "ctrl-alt-fn-f12");

        assert_eq!("esc".parse::<Keystroke>().unwrap().key, "escape");
        assert_eq!("cmd-return".parse::<Keystroke>().unwrap().key, "enter");
        assert_eq!("cmd-,".parse::<Keystroke>().unwrap().key, ",");
    }

    #[test]
    fn minus_can_be_the_key() {
        let k: Keystroke = "cmd--".parse().unwrap();
        assert!(k.cmd);
        assert_eq!(k.key, "-");
        let k: Keystroke = "-".parse().unwrap();
        assert_eq!(k.key, "-");
        assert_eq!(
            "cmd-shift--".parse::<Keystroke>().unwrap().to_string(),
            "shift-cmd--"
        );
    }

    #[test]
    fn parses_sequences() {
        let keys = parse_keys("cmd-k  cmd-s").unwrap();
        assert_eq!(keys.len(), 2);
        assert_eq!(format_keys(&keys), "cmd-k cmd-s");
    }

    #[test]
    fn rejects_invalid_keys() {
        assert_eq!(parse_keys("").unwrap_err(), KeystrokeError::Empty);
        assert_eq!(parse_keys("   ").unwrap_err(), KeystrokeError::Empty);
        assert_eq!(
            "cmd-cmd-d".parse::<Keystroke>().unwrap_err(),
            KeystrokeError::DuplicateModifier("cmd")
        );
        assert_eq!(
            "cmd-shift".parse::<Keystroke>().unwrap_err(),
            KeystrokeError::MissingKey("shift".into())
        );
        assert_eq!(
            "cmd-D".parse::<Keystroke>().unwrap_err(),
            KeystrokeError::UppercaseKey("D".into())
        );
        assert_eq!(
            "cmd-banana".parse::<Keystroke>().unwrap_err(),
            KeystrokeError::UnknownKey("banana".into())
        );
        assert!("cmd-f25".parse::<Keystroke>().is_err());
        assert!("cmd-f01".parse::<Keystroke>().is_err());
        assert!("super-d".parse::<Keystroke>().is_err());
        assert!("cmd-".parse::<Keystroke>().is_err());
    }

    #[test]
    fn missing_or_empty_file_yields_defaults() {
        let out = load_str(&defaults(), "", None).unwrap();
        assert_eq!(out.bindings, defaults());
        assert!(out.diagnostics.is_empty());

        let out = load(Path::new("/nonexistent/hutan/keymap.toml"), &defaults()).unwrap();
        assert_eq!(out.bindings, defaults());
    }

    #[test]
    fn user_bindings_are_added_and_override_defaults() {
        let text = r#"
[[binding]]
keys = "cmd-shift-d"
action = "workbench::SplitDownAlt"
context = "Pane"

[[binding]]
keys = "ctrl-a"
action = "workbench::Foo"
"#;
        let out = load_str(&defaults(), text, None).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert_eq!(
            out.bindings,
            vec![
                spec("cmd-d", "workbench::SplitRight", Some("Pane")),
                spec("cmd-k", "palette::Toggle", None),
                spec("cmd-shift-d", "workbench::SplitDownAlt", Some("Pane")),
                spec("ctrl-a", "workbench::Foo", None),
            ]
        );
    }

    #[test]
    fn none_unbinds_defaults() {
        let text = r#"
[[binding]]
keys = "cmd-d"
action = "none"
context = "Pane"

[[binding]]
keys = "cmd-k"
action = "none"
"#;
        let out = load_str(&defaults(), text, None).unwrap();
        assert!(out.diagnostics.is_empty(), "{:?}", out.diagnostics);
        assert_eq!(
            out.bindings,
            vec![spec("cmd-shift-d", "workbench::SplitDown", Some("Pane"))]
        );
    }

    #[test]
    fn none_with_context_only_unbinds_that_context() {
        let defaults = vec![
            spec("cmd-d", "a::A", Some("Pane")),
            spec("cmd-d", "b::B", Some("Terminal")),
        ];
        let text = "[[binding]]\nkeys = \"cmd-d\"\naction = \"none\"\ncontext = \"Pane\"\n";
        let out = load_str(&defaults, text, None).unwrap();
        assert_eq!(out.bindings, vec![spec("cmd-d", "b::B", Some("Terminal"))]);

        // Without a context, `none` unbinds the keys everywhere.
        let text = "[[binding]]\nkeys = \"cmd-d\"\naction = \"none\"\n";
        assert!(load_str(&defaults, text, None).unwrap().bindings.is_empty());
    }

    #[test]
    fn duplicate_user_bindings_conflict_and_last_wins() {
        let text = r#"
[[binding]]
keys = "ctrl-a"
action = "x::One"

[[binding]]
keys = "ctrl-a"
action = "x::Two"
"#;
        let out = load_str(&[], text, None).unwrap();
        assert_eq!(out.bindings, vec![spec("ctrl-a", "x::Two", None)]);
        assert_eq!(out.diagnostics.len(), 1);
        let d = &out.diagnostics[0];
        assert!(!d.is_error());
        assert!(d.message.contains("conflict"), "{}", d.message);
        assert!(d.message.contains("x::One") && d.message.contains("x::Two"));
    }

    #[test]
    fn same_keys_different_context_is_not_a_conflict() {
        let text = r#"
[[binding]]
keys = "ctrl-a"
action = "x::One"
context = "Pane"

[[binding]]
keys = "ctrl-a"
action = "x::Two"
context = "Terminal"
"#;
        let out = load_str(&[], text, None).unwrap();
        assert_eq!(out.bindings.len(), 2);
        assert!(out.diagnostics.is_empty());
    }

    #[test]
    fn conflicting_defaults_are_reported() {
        let defaults = vec![spec("cmd-d", "a::A", None), spec("cmd-d", "b::B", None)];
        let out = load_str(&defaults, "", None).unwrap();
        assert_eq!(out.bindings, vec![spec("cmd-d", "b::B", None)]);
        assert_eq!(out.diagnostics.len(), 1);
    }

    #[test]
    fn prefix_shadowing_is_a_warning() {
        let text = r#"
[[binding]]
keys = "cmd-k cmd-s"
action = "settings::Open"
"#;
        let out = load_str(&defaults(), text, None).unwrap();
        assert_eq!(out.bindings.len(), 4);
        assert_eq!(out.diagnostics.len(), 1);
        assert!(out.diagnostics[0].message.contains("prefix"));
    }

    #[test]
    fn invalid_bindings_are_skipped_with_errors() {
        let text = r#"
[[binding]]
keys = "cmd-banana"
action = "x::One"

[[binding]]
keys = "cmd-1"
action = "not an action"

[[binding]]
action = "x::NoKeys"

[[binding]]
keys = "cmd-2"

[[binding]]
keys = "cmd-3"
action = "x::Three"
bogus = true
"#;
        let path = Path::new("/tmp/keymap.toml");
        let out = load_str(&[], text, Some(path)).unwrap();
        assert_eq!(out.bindings, vec![spec("cmd-3", "x::Three", None)]);
        let paths: Vec<_> = out
            .diagnostics
            .iter()
            .map(|d| (d.is_error(), d.key_path.as_str()))
            .collect();
        assert_eq!(
            paths,
            vec![
                (true, "binding[0].keys"),
                (true, "binding[1].action"),
                (true, "binding[2].keys"),
                (true, "binding[3].action"),
                (false, "binding[4].bogus"),
            ]
        );
        assert!(
            out.diagnostics
                .iter()
                .all(|d| d.file.as_deref() == Some(path))
        );
    }

    #[test]
    fn invalid_toml_is_an_error_with_line() {
        let err = load_str(&[], "[[binding]\nkeys = 1\n", Some(Path::new("k.toml"))).unwrap_err();
        assert!(err.is_error());
        assert!(err.message.contains("line"), "{}", err.message);
    }

    #[test]
    fn unknown_top_level_key_warns() {
        let out = load_str(&[], "colour = 1\n", None).unwrap();
        assert_eq!(out.diagnostics.len(), 1);
        assert_eq!(out.diagnostics[0].key_path, "colour");
    }
}
