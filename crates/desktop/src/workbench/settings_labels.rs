//! What the settings sheet says: the user-facing title and help of every namespace and field,
//! and a label for every value of a choice. The schemas' doc comments are written for people who
//! edit `settings.toml` (they name keys, types and files); the sheet shows these instead, and
//! `docs/design.md` § Settings keeps the same wording. Pure data: no GPUI.
//!
//! A field without an entry here falls back to its key in Title Case and the schema's doc; the
//! tests fail for every field and choice of the built-in namespaces that has no entry, so the
//! fallback only ever shows for a namespace somebody added without wording.

/// The text of one field.
struct FieldText {
    namespace: &'static str,
    key: &'static str,
    title: &'static str,
    help: &'static str,
    /// `(value, label)` of every choice of a dropdown.
    values: &'static [(&'static str, &'static str)],
}

const fn field(
    namespace: &'static str,
    key: &'static str,
    title: &'static str,
    help: &'static str,
) -> FieldText {
    FieldText {
        namespace,
        key,
        title,
        help,
        values: &[],
    }
}

const fn choice(
    namespace: &'static str,
    key: &'static str,
    title: &'static str,
    help: &'static str,
    values: &'static [(&'static str, &'static str)],
) -> FieldText {
    FieldText {
        namespace,
        key,
        title,
        help,
        values,
    }
}

/// The title of a namespace in the sheet's list and header.
const NAMESPACES: &[(&str, &str)] = &[
    ("appearance", "Appearance"),
    ("app", "General"),
    ("keymap", "Keyboard"),
    ("terminal", "Terminal"),
    ("layout", "Layout"),
    ("panels.sessions", "Projects Sidebar"),
    ("panels.inspector", "Inspector"),
    ("panels.overview", "Overview"),
    ("workspaces", "Workspaces"),
    ("history", "History"),
    ("chat", "Chat"),
    ("editor", "Editing"),
    ("agents", "Agents"),
    ("resources", "Resources"),
    ("tasks", "Tasks"),
    ("assistant", "Assistant"),
    ("integrations", "Agent Integrations"),
    ("accounts", "Accounts"),
    ("models", "Models"),
];

const FIELDS: &[FieldText] = &[
    // ---- appearance ----
    choice(
        "appearance",
        "density",
        "Density",
        "How much space rows, buttons and panels take. Compact fits more on screen; Comfortable uses larger rows and more padding.",
        &[("compact", "Compact"), ("comfortable", "Comfortable")],
    ),
    choice(
        "appearance",
        "appearance",
        "Color mode",
        "Light or dark windows. System follows macOS and switches with it.",
        &[("system", "System"), ("light", "Light"), ("dark", "Dark")],
    ),
    // ---- general ----
    field(
        "app",
        "keep_running",
        "Keep the daemon running after Hutan quits",
        "Quitting Hutan leaves its background daemon and your sessions running, without asking. You can stop it from the Daemon menu or with “hutan daemon stop”. Off, the daemon stops when Hutan quits.",
    ),
    field(
        "app",
        "start_at_login",
        "Start the daemon when you log in",
        "Registers Hutan's launch agent in macOS Login Items so the daemon starts at login and restarts after a crash. While it is registered, quitting Hutan never stops the daemon. Only available when Hutan runs from Hutan.app.",
    ),
    field(
        "app",
        "setup_done",
        "First-run setup finished",
        "Whether the first-run setup was completed or skipped. Turn it off to see the setup again at the next start, or use “Run setup again” below.",
    ),
    choice(
        "app",
        "on_last_window_close",
        "When the last window closes",
        "Quit Hutan, which asks first while sessions are running, or keep Hutan in the menu bar without a Dock icon. Keeping it in the menu bar needs the menu bar item below.",
        &[("quit", "Quit Hutan"), ("menu_bar", "Keep in the menu bar")],
    ),
    field(
        "app",
        "menu_bar",
        "Show the menu bar item",
        "A menu bar icon with the daemon's state, how many sessions need you, and a menu to jump to them. While no Hutan window is connected, the daemon shows its own icon whatever this says.",
    ),
    field(
        "app",
        "dock_badge",
        "Show a badge on the Dock icon",
        "Shows how many sessions need you (blocked, waiting for an approval, failed or unread) on the Dock icon.",
    ),
    // ---- keyboard ----
    choice(
        "keymap",
        "base",
        "Key bindings",
        "The set of shortcuts Hutan starts from. Zed keys follow Zed on macOS (⌘⇧P command palette, ⌘P jump, ⌘B ⌘R ⌘J docks); Hutan keys are the original ones (⌘K command palette, ⌘J jump). Your own bindings in keymap.toml are applied on top, and a change takes effect at once.",
        &[("zed", "Zed keys"), ("hutan", "Hutan keys")],
    ),
    // ---- terminal ----
    field(
        "terminal",
        "font_family",
        "Font",
        "The font of terminal text. Use a monospaced font.",
    ),
    field(
        "terminal",
        "font_size",
        "Font size",
        "Terminal text size in points.",
    ),
    field(
        "terminal",
        "line_height",
        "Line height",
        "Space between terminal lines, as a multiple of the font size.",
    ),
    field(
        "terminal",
        "theme",
        "Color theme",
        "The terminal's colors: “auto” follows the system light or dark mode, or name a theme such as hutan-dark or hutan-light.",
    ),
    choice(
        "terminal",
        "cursor.style",
        "Cursor shape",
        "The shape of the cursor. A program that asks for another shape gets it.",
        &[
            ("block", "Block"),
            ("bar", "Bar"),
            ("underline", "Underline"),
        ],
    ),
    field(
        "terminal",
        "cursor.blink",
        "Blinking cursor",
        "Whether the cursor blinks while the terminal has keyboard focus.",
    ),
    choice(
        "terminal",
        "option_as_meta",
        "Option key as Meta",
        "Makes the Option key send Meta (Alt) for shell and editor shortcuts instead of typing special characters. macOS cannot tell the two Option keys apart here, so every choice except None applies to both.",
        &[
            ("none", "None"),
            ("left", "Left Option"),
            ("right", "Right Option"),
            ("both", "Both Option keys"),
        ],
    ),
    field(
        "terminal",
        "copy_on_select",
        "Copy on select",
        "Copies selected text to the clipboard as soon as you release the mouse.",
    ),
    field(
        "terminal",
        "scroll_to_bottom_on_input",
        "Jump to the latest output when typing",
        "When you type while scrolled back into history, return to the live screen.",
    ),
    field(
        "terminal",
        "blocks",
        "Command blocks",
        "Groups each command and its output into a block with a marker bar, a status chip, and actions on hover such as copy and fold. Off, the terminal looks like a plain terminal.",
    ),
    field(
        "terminal",
        "shell",
        "Shell",
        "The shell new terminals run: a full path or a name found on your PATH. Leave it empty to use your account's login shell.",
    ),
    field(
        "terminal",
        "env",
        "Extra environment variables",
        "Variables added to the environment of new terminals, on top of your login environment. They can hold secrets, so they are sent only to the daemon. Edit them in settings.toml.",
    ),
    // ---- layout ----
    field(
        "layout",
        "preset",
        "Layout preset",
        "The layout preset applied when Hutan starts and whenever you change this. Built in: default, focus and overview.",
    ),
    field(
        "layout",
        "presets",
        "Your layout presets",
        "Layouts you saved, by name. The built-in ones can be overridden here. Edit them in settings.toml, or save the current layout from the command palette.",
    ),
    // ---- projects sidebar ----
    choice(
        "panels.sessions",
        "group_by",
        "Group sessions by",
        "How the sidebar groups sessions.",
        &[
            ("workspace", "Workspace"),
            ("state", "State"),
            ("none", "Not grouped"),
        ],
    ),
    field(
        "panels.sessions",
        "needs_you",
        "“Needs you” section",
        "Shows a section on top with the sessions that are waiting for you.",
    ),
    field(
        "panels.sessions",
        "row",
        "Row contents",
        "What each row shows, in order. The first line can show state, kind, title and age; the second line can show folder (where the session is now) and placement (main checkout or worktree branch).",
    ),
    // ---- inspector ----
    field(
        "panels.inspector",
        "tabs",
        "Tabs",
        "The inspector's tabs, in order: approvals, changes and info.",
    ),
    field(
        "panels.inspector",
        "reveal_on_approval",
        "Open when an approval arrives",
        "Shows the inspector as soon as an agent asks for an approval.",
    ),
    // ---- overview ----
    choice(
        "panels.overview",
        "card_size",
        "Card size",
        "How large each session's card is on the overview board.",
        &[("small", "Small"), ("medium", "Medium"), ("large", "Large")],
    ),
    choice(
        "panels.overview",
        "preview",
        "Card preview",
        "What a card shows of the terminal: a live view, a still picture, or nothing.",
        &[("live", "Live"), ("static", "Still"), ("off", "Off")],
    ),
    choice(
        "panels.overview",
        "sort",
        "Order cards by",
        "How the cards are ordered.",
        &[
            ("attention", "Needs attention first"),
            ("recent", "Most recent"),
            ("name", "Name"),
        ],
    ),
    // ---- workspaces ----
    choice(
        "workspaces",
        "sort",
        "Sort by",
        "How projects and workspaces are ordered in the sidebar, within each level.",
        &[
            ("manual", "Manual order"),
            ("recent", "Most recent"),
            ("attention", "Needs attention first"),
            ("name", "Name"),
        ],
    ),
    field(
        "workspaces",
        "hide_empty",
        "Hide projects without sessions",
        "Leaves out projects that have no session at all.",
    ),
    field(
        "workspaces",
        "hide_sleeping",
        "Hide projects without a running session",
        "Leaves out projects that have no session running right now.",
    ),
    field(
        "workspaces",
        "hide_cli_created",
        "Hide projects made by the command line",
        "Leaves out projects that the hutan command or an agent created.",
    ),
    field(
        "workspaces",
        "always_show_active",
        "Always show where you are",
        "Keeps the project and workspace you are in on the list even when a filter above would hide them.",
    ),
    // ---- history ----
    field(
        "history",
        "sources",
        "Search these sources by default",
        "Which history sources the search starts with: hutan, claude, codex or omp. Leave empty to search all of them.",
    ),
    field(
        "history",
        "roles",
        "Search these kinds of text by default",
        "Which kinds of text the search starts with: user, assistant, tool, command or output. Leave empty to search all of them.",
    ),
    choice(
        "history",
        "range",
        "How far back to search",
        "The time range the search starts with.",
        &[
            ("24h", "Last 24 hours"),
            ("7d", "Last 7 days"),
            ("30d", "Last 30 days"),
            ("all", "All time"),
        ],
    ),
    choice(
        "history",
        "project",
        "Search in",
        "Whether the search covers the workspace you are in or every project.",
        &[
            ("workspace", "The current workspace"),
            ("all", "All projects"),
        ],
    ),
    field(
        "history",
        "show_snippets",
        "Show matching text",
        "Shows the matching text under each search result.",
    ),
    // ---- chat ----
    field(
        "chat",
        "send_on_enter",
        "Send with Return",
        "Return sends the message and Shift-Return starts a new line. Off, Return starts a new line and ⌘-Return sends.",
    ),
    field(
        "chat",
        "attach_max_bytes",
        "Largest block to attach (bytes)",
        "The most output of a terminal block that “Send to agent” attaches to a message. Longer output is cut, and the attachment says so.",
    ),
    // ---- editing ----
    field(
        "editor",
        "vim_mode",
        "Vim mode",
        "Vim-style editing (Normal, Insert and Visual modes) in Hutan's text fields. Fields start in Insert mode; Escape switches to Normal, and Escape again does what it always did, such as closing a dialog. Terminals are not affected.",
    ),
    field(
        "editor",
        "vim_use_system_clipboard",
        "Vim uses the system clipboard",
        "Copies and deletes in Vim mode also go to the system clipboard, and p pastes from it. Off, they use a private register in each field.",
    ),
    // ---- agents ----
    field(
        "agents",
        "show_finished",
        "Show finished agents",
        "Lists agents that have finished, dimmed, with their sub-agents folded into one row. Off, only agents still around are listed.",
    ),
    field(
        "agents",
        "only_blocked",
        "Only blocked agents",
        "Lists only the agents that are blocked, plus the ones above them.",
    ),
    choice(
        "agents",
        "scope",
        "Show agents from",
        "Whether the monitor lists agents of the current workspace or of every workspace.",
        &[
            ("active_workspace", "The current workspace"),
            ("all", "All workspaces"),
        ],
    ),
    // ---- resources ----
    field(
        "resources",
        "interval_ms",
        "Refresh interval (milliseconds)",
        "How often the daemon measures processes while the Resources panel is open; at least 1000. Nothing is measured while the panel is hidden.",
    ),
    choice(
        "resources",
        "sort",
        "Sort by",
        "How sessions, and the processes inside each, are ordered. The column headers change it too.",
        &[("cpu", "CPU"), ("memory", "Memory"), ("name", "Name")],
    ),
    // ---- tasks ----
    field(
        "tasks",
        "show_done",
        "Show finished tasks",
        "Lists tasks that are done or cancelled. Off, they are hidden and the tasks that waited for them move up; failed tasks always stay.",
    ),
    choice(
        "tasks",
        "scope",
        "Show tasks from",
        "Whether the Tasks panel lists tasks of the current workspace or of every workspace.",
        &[
            ("workspace", "The current workspace"),
            ("all", "All workspaces"),
        ],
    ),
    // ---- assistant ----
    field(
        "assistant",
        "enabled",
        "Use the assistant",
        "Off hides the Assistant panel, removes its commands and shortcuts, and releases the system-wide shortcut. Your earlier assistant conversations are kept.",
    ),
    field(
        "assistant",
        "global_shortcut",
        "System-wide shortcut",
        "A shortcut that brings Hutan forward and opens the Assistant from any app, for example alt-space or cmd-shift-space. It needs at least one of cmd, ctrl or alt. Leave it empty to turn it off. If another app already uses it, Hutan tells you once and leaves the setting alone.",
    ),
    // ---- agent integrations ----
    field(
        "integrations",
        "auto_install",
        "Install agent hooks automatically",
        "When Hutan starts, adds its hooks to Codex, Claude Code and OMP, for each one that is installed and not yet connected. Only Hutan's own entries are written, and the files are backed up first. The hooks do nothing outside a Hutan session. Turning this off stops future installs but removes nothing; use “hutan integration uninstall” to remove them.",
    ),
];

fn entry(namespace: &str, key: &str) -> Option<&'static FieldText> {
    FIELDS
        .iter()
        .find(|f| f.namespace == namespace && f.key == key)
}

/// `font_size` → `Font Size`: the fallback for a key without an entry.
fn humanize(key: &str) -> String {
    key.rsplit('.')
        .next()
        .unwrap_or(key)
        .split(['_', '-'])
        .filter(|w| !w.is_empty())
        .map(|word| {
            let mut chars = word.chars();
            chars.next().map_or_else(String::new, |first| {
                first.to_uppercase().chain(chars).collect::<String>()
            })
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// The title of `namespace` in the sheet.
pub fn namespace_title(namespace: &str) -> String {
    NAMESPACES
        .iter()
        .find(|(ns, _)| *ns == namespace)
        .map_or_else(|| humanize(namespace), |(_, title)| (*title).to_owned())
}

/// The title of a field; `key` is its dotted path inside the namespace.
pub fn field_title(namespace: &str, key: &str) -> String {
    entry(namespace, key).map_or_else(|| humanize(key), |f| f.title.to_owned())
}

/// The help under a field; `doc` is the schema's own text, used only when there is no entry.
pub fn field_help(namespace: &str, key: &str, doc: &str) -> String {
    entry(namespace, key).map_or_else(|| doc.to_owned(), |f| f.help.to_owned())
}

/// The label of one value of a choice field.
pub fn value_label(namespace: &str, key: &str, value: &str) -> String {
    entry(namespace, key)
        .and_then(|f| f.values.iter().find(|(v, _)| *v == value))
        .map_or_else(|| humanize(value), |(_, label)| (*label).to_owned())
}

/// Everything a search of the sheet matches in `namespace`'s field: title, help and the labels
/// of its choices (the key itself matches too, for people who know it).
pub fn field_search_text(namespace: &str, key: &str, doc: &str) -> String {
    let mut text = format!(
        "{} {}",
        field_title(namespace, key),
        field_help(namespace, key, doc)
    );
    if let Some(f) = entry(namespace, key) {
        for (_, label) in f.values {
            text.push(' ');
            text.push_str(label);
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workbench::settings_schema::{FieldKind, catalog, fields_of, namespace_schema};
    use hutan_settings::{SettingsPaths, SettingsStore};

    /// A word with an underscore between lowercase parts (`on_last_window_close`).
    fn has_snake_case(text: &str) -> bool {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .any(|word| {
                word.contains('_')
                    && word
                        .chars()
                        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
                    && word.trim_matches('_').contains('_')
            })
    }

    fn assert_user_facing(what: &str, text: &str) {
        assert!(!text.trim().is_empty(), "{what}: empty");
        assert!(!text.contains("::"), "{what}: `{text}` has `::`");
        assert!(!text.contains('`'), "{what}: `{text}` has a code span");
        assert!(!has_snake_case(text), "{what}: `{text}` has a key name");
        assert!(
            !text.contains("docs/"),
            "{what}: `{text}` names a repo file"
        );
    }

    #[test]
    fn every_namespace_field_and_choice_has_user_facing_text() {
        let dir = tempfile::tempdir().unwrap();
        let store = SettingsStore::new(SettingsPaths::in_dir(dir.path()));
        crate::settings::register_all(&store);
        let schema = store.schema();
        let mut fields = 0;
        for entry in catalog() {
            let ns = entry.namespace;
            assert!(
                NAMESPACES.iter().any(|(n, _)| *n == ns),
                "namespace {ns} has no title"
            );
            assert_user_facing(&format!("namespace {ns}"), &namespace_title(ns));
            let Some(namespace_schema) = namespace_schema(&schema, ns) else {
                continue;
            };
            for field in fields_of(&namespace_schema) {
                let key = field.dotted();
                let what = format!("{ns}.{key}");
                assert!(self::entry(ns, &key).is_some(), "{what} has no wording");
                assert_user_facing(&format!("{what} title"), &field_title(ns, &key));
                assert_user_facing(&format!("{what} help"), &field_help(ns, &key, &field.doc));
                if let FieldKind::Enum(values) = &field.kind {
                    for value in values {
                        assert!(
                            self::entry(ns, &key)
                                .is_some_and(|f| f.values.iter().any(|(v, _)| v == value)),
                            "{what}: choice `{value}` has no label"
                        );
                        assert_user_facing(
                            &format!("{what} choice {value}"),
                            &value_label(ns, &key, value),
                        );
                    }
                }
                fields += 1;
            }
        }
        assert!(fields > 50, "only {fields} fields were checked");
        // Nothing in the table is left over from a field that is gone.
        for f in FIELDS {
            let exists = catalog()
                .iter()
                .filter(|e| e.namespace == f.namespace)
                .filter_map(|e| namespace_schema(&schema, e.namespace))
                .flat_map(|s| fields_of(&s))
                .any(|field| field.dotted() == f.key);
            assert!(exists, "{}.{} is not a field", f.namespace, f.key);
        }
    }

    #[test]
    fn the_fallbacks_read_like_titles() {
        assert_eq!(humanize("font_size"), "Font Size");
        assert_eq!(humanize("cursor.blink"), "Blink");
        assert_eq!(field_title("terminal", "font_size"), "Font size");
        assert_eq!(
            value_label("app", "on_last_window_close", "menu_bar"),
            "Keep in the menu bar"
        );
        assert_eq!(value_label("nowhere", "x", "some_value"), "Some Value");
    }

    #[test]
    fn the_checker_rejects_what_it_is_for() {
        assert!(has_snake_case("see on_last_window_close"));
        assert!(!has_snake_case("Font size, in points."));
    }
}
