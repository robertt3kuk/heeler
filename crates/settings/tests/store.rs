//! Store semantics over real files in temp dirs (no watcher).
#![allow(clippy::unwrap_used, clippy::expect_used)] // test helpers outside #[test] fns

mod common;

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;

use common::*;
use heeler_settings::{
    AppearanceSettings, Density, SettingsPaths, SettingsStore, Severity, keymap,
};
use tempfile::TempDir;

struct Fixture {
    dir: TempDir,
    store: SettingsStore,
}

impl Fixture {
    fn new(user: Option<&str>) -> Self {
        let dir = TempDir::new().unwrap();
        let paths = SettingsPaths::in_dir(dir.path());
        if let Some(user) = user {
            write_file(&paths.user, user);
        }
        let store = SettingsStore::new(paths);
        Self { dir, store }
    }

    fn user_path(&self) -> std::path::PathBuf {
        self.dir.path().join("settings.toml")
    }

    fn workspace(&self, text: &str) -> std::path::PathBuf {
        let root = self.dir.path().join("repo");
        write_file(&root.join(".heeler/settings.toml"), text);
        self.store.set_workspace_root(Some(root.clone()));
        root
    }
}

#[test]
fn defaults_when_nothing_is_configured() {
    let f = Fixture::new(None);
    assert_eq!(*f.store.get::<Terminal>(), Terminal::default());
    assert_eq!(
        *f.store.get::<AppearanceSettings>(),
        AppearanceSettings::default()
    );
    assert!(f.store.errors().is_empty());
}

#[test]
fn user_layer_overrides_defaults_and_merges_deeply() {
    let f = Fixture::new(Some(
        "[terminal]\nfont_size = 15\ntags = [\"x\", \"y\"]\n[terminal.cursor]\nblink = false\n",
    ));
    let t = f.store.get::<Terminal>();
    assert_eq!(t.font_size, 15);
    assert_eq!(t.font_family, "Menlo", "untouched keys keep defaults");
    assert!(!t.cursor.blink);
    assert_eq!(
        t.cursor.style, "block",
        "nested defaults survive a partial nested table"
    );
    assert_eq!(t.tags, ["x", "y"], "arrays replace, not append");
    assert!(f.store.errors().is_empty(), "{:?}", f.store.errors());
}

#[test]
fn precedence_is_defaults_user_workspace_runtime() {
    let f = Fixture::new(Some(
        "[layout]\npreset = \"user\"\n[layout.presets.a]\nsize = 1\npanels = [\"p\"]\n[layout.presets.b]\nsize = 2\n",
    ));
    f.workspace(
        "[layout]\n[layout.presets.a]\nsize = 10\n[layout.presets.c]\nsize = 3\n[panels.sessions]\ngroup_by = \"state\"\n",
    );

    let l = f.store.get::<Layout>();
    assert_eq!(
        l.preset, "user",
        "workspace did not set preset; user wins over default"
    );
    assert_eq!(l.presets["a"].size, 10, "workspace beats user");
    assert_eq!(
        l.presets["a"].panels,
        ["p"],
        "deep merge keeps user keys the workspace did not set"
    );
    assert_eq!(l.presets["b"].size, 2);
    assert_eq!(l.presets["c"].size, 3);
    let s = f.store.get::<Sessions>();
    assert_eq!(s.group_by, "state");
    assert!(s.needs_you);
    assert!(f.store.errors().is_empty(), "{:?}", f.store.errors());

    f.store.set_runtime(Layout {
        preset: "runtime".into(),
        presets: Default::default(),
    });
    assert_eq!(
        f.store.get::<Layout>().preset,
        "runtime",
        "runtime beats workspace and user"
    );
    assert_eq!(
        f.store.get::<Layout>().presets["a"].size,
        10,
        "runtime table merges deeply too"
    );
}

#[test]
fn workspace_override_allowed_for_overridable_namespaces() {
    let f = Fixture::new(Some("[layout]\npreset = \"user\"\n"));
    f.workspace("[layout]\npreset = \"ws\"\n");
    assert_eq!(f.store.get::<Layout>().preset, "ws");
    assert!(f.store.errors().is_empty());

    f.store.set_workspace_root(None);
    assert_eq!(f.store.get::<Layout>().preset, "user");
}

#[test]
fn workspace_override_refused_for_other_namespaces() {
    let f = Fixture::new(Some("[terminal]\nfont_size = 15\n"));
    f.store.register::<Terminal>();
    let root = f.workspace("[terminal]\nfont_size = 99\n");

    assert_eq!(
        f.store.get::<Terminal>().font_size,
        15,
        "workspace value ignored"
    );
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].severity, Severity::Error);
    assert_eq!(errors[0].key_path, "terminal");
    assert_eq!(
        errors[0].file.as_deref(),
        Some(root.join(".heeler/settings.toml").as_path())
    );
    assert!(
        errors[0].message.contains("workspace"),
        "{}",
        errors[0].message
    );
}

#[test]
fn invalid_value_at_startup_falls_back_and_reports_the_key() {
    let f = Fixture::new(Some(
        "[terminal]\nfont_size = \"big\"\nfont_family = \"Fira\"\n",
    ));
    let t = f.store.get::<Terminal>();
    assert_eq!(*t, Terminal::default(), "no last good value yet: defaults");
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "terminal.font_size");
    assert_eq!(errors[0].file.as_deref(), Some(f.user_path().as_path()));
    assert!(errors[0].is_error());
}

#[test]
fn invalid_value_keeps_last_good_value_and_recovers() {
    let f = Fixture::new(Some("[terminal]\nfont_size = 15\n"));
    assert_eq!(f.store.get::<Terminal>().font_size, 15);

    f.store
        .write_user("terminal.font_size", toml::Value::String("huge".into()))
        .unwrap();
    assert_eq!(
        f.store.get::<Terminal>().font_size,
        15,
        "last good value kept"
    );
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "terminal.font_size");

    f.store
        .write_user("terminal.font_size", toml::Value::Integer(17))
        .unwrap();
    assert_eq!(f.store.get::<Terminal>().font_size, 17);
    assert!(f.store.errors().is_empty());
}

#[test]
fn namespace_that_is_not_a_table_is_an_error() {
    let f = Fixture::new(Some("terminal = 5\n"));
    assert_eq!(*f.store.get::<Terminal>(), Terminal::default());
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "terminal");
}

#[test]
fn unknown_keys_warn_without_failing() {
    let f = Fixture::new(Some(
        "[terminal]\nfont_size = 15\nbogus = 1\n[terminal.cursor]\nshape = \"x\"\n\n[nothing_here]\nx = 1\n",
    ));
    assert_eq!(
        f.store.get::<Terminal>().font_size,
        15,
        "value still applies"
    );
    let mut errors = f.store.errors();
    errors.sort_by(|a, b| a.key_path.cmp(&b.key_path));
    let summary: Vec<_> = errors
        .iter()
        .map(|e| (e.severity, e.key_path.as_str()))
        .collect();
    assert_eq!(
        summary,
        [
            (Severity::Warning, "nothing_here"),
            (Severity::Warning, "terminal.bogus"),
            (Severity::Warning, "terminal.cursor.shape"),
        ]
    );
    assert!(
        errors
            .iter()
            .all(|e| e.file.as_deref() == Some(f.user_path().as_path()))
    );
}

#[test]
fn unknown_namespace_warning_disappears_once_registered() {
    let f = Fixture::new(Some("[layout]\npreset = \"x\"\n"));
    f.store.register::<Terminal>();
    assert_eq!(f.store.errors().len(), 1);
    assert_eq!(f.store.errors()[0].key_path, "layout");
    f.store.register::<Layout>();
    assert!(f.store.errors().is_empty());
    assert_eq!(f.store.get::<Layout>().preset, "x");
}

#[test]
fn nested_namespaces_do_not_flag_their_parent_table() {
    let f = Fixture::new(Some(
        "[panels.sessions]\ngroup_by = \"none\"\n[panels.other]\nx = 1\n",
    ));
    f.store.register::<Sessions>();
    assert_eq!(f.store.get::<Sessions>().group_by, "none");
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "panels.other");
}

#[test]
fn invalid_toml_keeps_previous_layer_and_reports_line() {
    let f = Fixture::new(Some("[terminal]\nfont_size = 15\n"));
    assert_eq!(f.store.get::<Terminal>().font_size, 15);
    write_file(&f.user_path(), "[terminal]\nfont_size = \n");
    // No watcher here: a second store over the same broken file starts from defaults + error.
    let again = SettingsStore::new(SettingsPaths::in_dir(f.dir.path()));
    assert_eq!(again.get::<Terminal>().font_size, 13);
    let errors = again.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(
        errors[0].message.contains("line 2"),
        "{}",
        errors[0].message
    );
    assert_eq!(errors[0].key_path, "");
}

#[test]
fn write_user_creates_file_and_private_directory() {
    let dir = TempDir::new().unwrap();
    let base = dir.path().join("nested/Heeler");
    let store = SettingsStore::new(SettingsPaths::in_dir(&base));
    store
        .write_user(
            "appearance.density",
            toml::Value::String("comfortable".into()),
        )
        .unwrap();

    let text = std::fs::read_to_string(base.join("settings.toml")).unwrap();
    assert_eq!(text, "[appearance]\ndensity = \"comfortable\"\n");
    let mode = std::fs::metadata(&base).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o700);
    assert_eq!(
        store.get::<AppearanceSettings>().density,
        Density::Comfortable,
        "applied without waiting for a watcher"
    );
}

#[test]
fn write_user_preserves_comments_and_formatting() {
    const ORIGINAL: &str = "\
# Heeler settings
# Hand-edited; keep my notes.

[terminal]
# the font
font_family = \"Fira Code\"   # not Menlo
font_size = 13 # readable

   [layout]   # trailing on header
   preset = \"default\"
";
    let f = Fixture::new(Some(ORIGINAL));
    f.store
        .write_user("terminal.font_size", toml::Value::Integer(16))
        .unwrap();
    f.store
        .write_user("terminal.cursor.blink", toml::Value::Boolean(false))
        .unwrap();

    let text = std::fs::read_to_string(f.user_path()).unwrap();
    let expected = "\
# Heeler settings
# Hand-edited; keep my notes.

[terminal]
# the font
font_family = \"Fira Code\"   # not Menlo
font_size = 16 # readable

[terminal.cursor]
blink = false

   [layout]   # trailing on header
   preset = \"default\"
";
    // toml_edit places the new sub-table after `[terminal]`'s keys; assert on the exact text so a
    // formatting regression is visible.
    assert_eq!(text, expected);
    assert!(!f.store.get::<Terminal>().cursor.blink);
    assert_eq!(f.store.get::<Terminal>().font_size, 16);
}

#[test]
fn write_user_refuses_to_clobber_a_broken_file() {
    let f = Fixture::new(Some("[terminal\n"));
    let broken = std::fs::read_to_string(f.user_path()).unwrap();
    let err = f
        .store
        .write_user("terminal.font_size", toml::Value::Integer(1))
        .unwrap_err();
    assert!(err.message.contains("invalid TOML"), "{}", err.message);
    assert_eq!(std::fs::read_to_string(f.user_path()).unwrap(), broken);
}

#[test]
fn write_user_follows_symlinked_settings_file() {
    let dir = TempDir::new().unwrap();
    let real = dir.path().join("dotfiles/settings.toml");
    write_file(&real, "# mine\n[terminal]\nfont_size = 12\n");
    let base = dir.path().join("app");
    std::fs::create_dir_all(&base).unwrap();
    std::os::unix::fs::symlink(&real, base.join("settings.toml")).unwrap();

    let store = SettingsStore::new(SettingsPaths::in_dir(&base));
    store
        .write_user("terminal.font_size", toml::Value::Integer(14))
        .unwrap();
    assert!(
        base.join("settings.toml")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        std::fs::read_to_string(&real).unwrap(),
        "# mine\n[terminal]\nfont_size = 14\n"
    );
}

#[test]
fn changes_report_changed_namespaces_only() {
    let f = Fixture::new(None);
    f.store.register::<Terminal>();
    f.store.register::<Layout>();
    let mut rx = f.store.changes();
    assert_eq!(rx.borrow_and_update().generation, 0);

    f.store
        .write_user("terminal.font_size", toml::Value::Integer(20))
        .unwrap();
    assert!(rx.has_changed().unwrap());
    let cs = rx.borrow_and_update().clone();
    assert_eq!(cs.generation, 1);
    assert_eq!(cs.namespaces.iter().collect::<Vec<_>>(), ["terminal"]);

    // Same value again: nothing changed, nothing published.
    f.store
        .write_user("terminal.font_size", toml::Value::Integer(20))
        .unwrap();
    assert!(!rx.has_changed().unwrap());

    f.store.set_runtime(Layout {
        preset: "r".into(),
        presets: Default::default(),
    });
    let cs = rx.borrow_and_update().clone();
    assert_eq!(cs.generation, 2);
    assert_eq!(cs.namespaces.iter().collect::<Vec<_>>(), ["layout"]);
}

#[test]
fn get_returns_a_shared_arc_until_something_changes() {
    let f = Fixture::new(None);
    let a = f.store.get::<Terminal>();
    let b = f.store.get::<Terminal>();
    assert!(Arc::ptr_eq(&a, &b));
    f.store
        .write_user("layout.preset", toml::Value::String("x".into()))
        .unwrap();
    assert!(
        Arc::ptr_eq(&a, &f.store.get::<Terminal>()),
        "unrelated namespace untouched"
    );
}

#[test]
fn schema_contains_registered_namespaces() {
    let f = Fixture::new(None);
    f.store.register::<Terminal>();
    f.store.register::<Layout>();
    f.store.register::<Sessions>();
    f.store.register::<AppearanceSettings>();
    let schema = f.store.schema();
    let namespaces = schema["namespaces"].as_object().unwrap();
    let mut names: Vec<_> = namespaces.keys().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        ["appearance", "layout", "panels.sessions", "terminal"]
    );

    assert_eq!(namespaces["terminal"]["workspace_overridable"], false);
    assert_eq!(namespaces["layout"]["workspace_overridable"], true);
    let props = &namespaces["terminal"]["schema"]["properties"];
    assert!(props["font_size"].is_object(), "{props}");
    assert!(props["font_family"].is_object());
    let density = &namespaces["appearance"]["schema"];
    assert!(density.to_string().contains("comfortable"), "{density}");
}

#[test]
fn conflicting_namespace_registration_is_reported_and_ignored() {
    let f = Fixture::new(None);
    f.store.register::<Terminal>();
    f.store.register::<Impostor>();
    assert_eq!(
        f.store.get::<Impostor>().x,
        0,
        "impostor only ever sees defaults"
    );
    assert_eq!(f.store.get::<Terminal>().font_size, 13);
    let errors = f.store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert!(errors[0].message.contains("already registered"));
}

#[test]
fn keymap_file_errors_surface_and_keymap_merges_with_defaults() {
    let f = Fixture::new(None);
    let keymap_path = f.dir.path().join("keymap.toml");
    write_file(
        &keymap_path,
        "[[binding]]\nkeys = \"cmd-d\"\naction = \"none\"\n[[binding]]\nkeys = \"cmd-wat\"\naction = \"x::Y\"\n",
    );
    let store = SettingsStore::new(SettingsPaths::in_dir(f.dir.path()));
    let defaults = vec![
        keymap::BindingSpec::new("cmd-d", "workbench::SplitRight", None).unwrap(),
        keymap::BindingSpec::new("cmd-t", "workbench::NewTerminal", None).unwrap(),
    ];
    let loaded = store.keymap(&defaults);
    assert_eq!(loaded.bindings.len(), 1);
    assert_eq!(loaded.bindings[0].action, "workbench::NewTerminal");
    let errors = store.errors();
    assert_eq!(errors.len(), 1, "{errors:?}");
    assert_eq!(errors[0].key_path, "binding[1].keys");
    assert_eq!(errors[0].file.as_deref(), Some(Path::new(&keymap_path)));
}

#[test]
fn paths_resolve_from_heeler_home_or_app_support() {
    let p = SettingsPaths::resolve(Some("/h".into()), Some("/Users/u".into())).unwrap();
    assert_eq!(p.user, Path::new("/h/settings.toml"));
    assert_eq!(p.keymap, Path::new("/h/keymap.toml"));
    let p = SettingsPaths::resolve(Some("".into()), Some("/Users/u".into())).unwrap();
    assert_eq!(
        p.user,
        Path::new("/Users/u/Library/Application Support/Heeler/settings.toml")
    );
    assert!(SettingsPaths::resolve(None, None).is_err());
}
