//! The layered store: defaults < user < workspace < runtime, validated per namespace.

use std::any::{Any, TypeId};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::{Mutex, RwLock};
use serde_json::Value as Json;
use tokio::sync::watch;
use toml::Value;

use crate::error::{SettingsError, syntax_error};
use crate::value::{deep_merge, join, json_to_toml, subtree, unknown_keys};
use crate::watch::{WatchGuard, WatchState};
use crate::{Setting, edit, keymap};

/// Namespace reported in [`ChangeSet`] when the keymap file's accepted contents changed.
pub const KEYMAP_NAMESPACE: &str = "keymap";

/// Where the settings files live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettingsPaths {
    /// `settings.toml`
    pub user: PathBuf,
    /// `keymap.toml`
    pub keymap: PathBuf,
}

impl SettingsPaths {
    /// `settings.toml` and `keymap.toml` inside `base`.
    pub fn in_dir(base: impl AsRef<Path>) -> Self {
        let base = base.as_ref();
        Self {
            user: base.join("settings.toml"),
            keymap: base.join("keymap.toml"),
        }
    }

    /// `$HEELER_HOME` (empty counts as unset), else `~/Library/Application Support/Heeler`.
    pub fn from_env() -> Result<Self, SettingsError> {
        Self::resolve(std::env::var_os("HEELER_HOME"), std::env::home_dir())
    }

    /// Pure form of [`SettingsPaths::from_env`].
    pub fn resolve(
        heeler_home: Option<std::ffi::OsString>,
        user_home: Option<PathBuf>,
    ) -> Result<Self, SettingsError> {
        match heeler_home.filter(|v| !v.is_empty()) {
            Some(home) => Ok(Self::in_dir(PathBuf::from(home))),
            None => {
                let home = user_home.ok_or_else(|| {
                    SettingsError::error(
                        None,
                        "",
                        "cannot determine the home directory; set HEELER_HOME",
                    )
                })?;
                Ok(Self::in_dir(
                    home.join("Library/Application Support/Heeler"),
                ))
            }
        }
    }
}

/// What changed in one store update. The channel only holds the latest value: a receiver that
/// sees `generation` jump by more than one missed intermediate sets and should re-read whatever
/// it cares about (`get` is cheap; compare with `Arc::ptr_eq`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChangeSet {
    /// Increases on every update, including updates that only changed diagnostics.
    pub generation: u64,
    /// Namespaces whose effective value changed, plus [`KEYMAP_NAMESPACE`] when the keymap file
    /// changed. Empty when only `errors()` changed.
    pub namespaces: BTreeSet<String>,
}

impl ChangeSet {
    pub fn contains(&self, namespace: &str) -> bool {
        self.namespaces.contains(namespace)
    }
}

type Erased = Arc<dyn Any + Send + Sync>;

#[derive(Clone)]
struct Computed {
    value: Erased,
    json: Json,
}

struct Loaded {
    computed: Computed,
    unknown: Vec<String>,
}

struct Entry {
    type_id: TypeId,
    overridable: bool,
    /// `T::default()` as TOML, the bottom layer of every merge.
    default: Value,
    initial: Computed,
    load: fn(&Value) -> Result<Loaded, String>,
    schema: Json,
}

fn load_typed<T: Setting>(value: &Value) -> Result<Loaded, String> {
    let typed: T = value.clone().try_into().map_err(|e| e.to_string())?;
    let json = serde_json::to_value(&typed).map_err(|e| e.to_string())?;
    let unknown = unknown_keys(value, &json);
    Ok(Loaded {
        computed: Computed {
            value: Arc::new(typed),
            json,
        },
        unknown,
    })
}

fn make_entry<T: Setting>() -> Result<Entry, String> {
    let initial = T::default();
    let json =
        serde_json::to_value(&initial).map_err(|e| format!("cannot serialize default: {e}"))?;
    let default =
        json_to_toml(&json).ok_or_else(|| "default is not representable as TOML".to_owned())?;
    if !default.is_table() {
        return Err("a setting must serialize to a table".to_owned());
    }
    let schema = serde_json::to_value(schemars::schema_for!(T))
        .map_err(|e| format!("cannot serialize schema: {e}"))?;
    Ok(Entry {
        type_id: TypeId::of::<T>(),
        overridable: T::WORKSPACE_OVERRIDABLE,
        default,
        initial: Computed {
            value: Arc::new(initial),
            json,
        },
        load: load_typed::<T>,
        schema,
    })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Keymap,
    Workspace,
}

/// One file on disk and its last accepted contents.
#[derive(Default)]
struct Layer {
    path: Option<PathBuf>,
    /// Last observed contents (`Some(None)` = file missing); skips re-parsing unchanged files.
    seen: Option<Option<String>>,
    /// Last good parse (settings files).
    table: toml::Table,
    /// Last good text (keymap).
    text: Option<String>,
    errors: Vec<SettingsError>,
}

impl Layer {
    fn at(path: Option<PathBuf>) -> Self {
        Self {
            path,
            ..Self::default()
        }
    }

    /// Re-reads the file. Returns whether the accepted contents changed. A file that cannot be
    /// read or parsed keeps the previous accepted contents and records an error.
    fn refresh(&mut self, kind: Kind) -> bool {
        let Some(path) = self.path.clone() else {
            return false;
        };
        let observed = match std::fs::read_to_string(&path) {
            Ok(text) => Some(text),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => {
                self.errors = vec![SettingsError::error(
                    Some(&path),
                    "",
                    format!("cannot read file: {e}"),
                )];
                return false;
            }
        };
        if self.seen.as_ref() == Some(&observed) {
            return false;
        }
        self.seen = Some(observed.clone());
        let Some(text) = observed else {
            self.errors.clear();
            let changed = !self.table.is_empty() || self.text.is_some();
            self.table.clear();
            self.text = None;
            return changed;
        };
        match kind {
            Kind::Keymap => match keymap::load_str(&[], &text, Some(&path)) {
                Ok(load) => {
                    self.errors = load.diagnostics;
                    let changed = self.text.as_deref() != Some(text.as_str());
                    self.text = Some(text);
                    changed
                }
                Err(e) => {
                    self.errors = vec![e];
                    false
                }
            },
            Kind::User | Kind::Workspace => match toml::from_str::<toml::Table>(&text) {
                Ok(table) => {
                    self.errors.clear();
                    let changed = self.table != table;
                    self.table = table;
                    changed
                }
                Err(e) => {
                    self.errors = vec![syntax_error(&path, &text, &e)];
                    false
                }
            },
        }
    }
}

struct State {
    user: Layer,
    keymap: Layer,
    workspace: Layer,
    workspace_root: Option<PathBuf>,
    registry: BTreeMap<&'static str, Entry>,
    runtime: BTreeMap<&'static str, Value>,
    values: BTreeMap<&'static str, Computed>,
    /// Diagnostics derived from the layers and registry by the last `recompute`.
    derived: Vec<SettingsError>,
    /// Registration problems (duplicate namespace with a different type, …).
    registration: Vec<SettingsError>,
    published_errors: Vec<SettingsError>,
    generation: u64,
}

fn workspace_file(root: &Path) -> PathBuf {
    root.join(".heeler").join("settings.toml")
}

impl State {
    fn errors(&self) -> Vec<SettingsError> {
        let mut out = Vec::new();
        out.extend(self.user.errors.iter().cloned());
        out.extend(self.keymap.errors.iter().cloned());
        out.extend(self.workspace.errors.iter().cloned());
        out.extend(self.registration.iter().cloned());
        out.extend(self.derived.iter().cloned());
        out
    }

    fn register<T: Setting>(&mut self) -> bool {
        let namespace = T::NAMESPACE;
        if let Some(existing) = self.registry.get(namespace) {
            if existing.type_id != TypeId::of::<T>() {
                let message = format!(
                    "namespace `{namespace}` is already registered by another type; ignoring `{}`",
                    std::any::type_name::<T>()
                );
                tracing::error!("{message}");
                let error = SettingsError::error(None, namespace, message);
                if !self.registration.contains(&error) {
                    self.registration.push(error);
                }
            }
            return false;
        }
        if namespace == KEYMAP_NAMESPACE
            || namespace.is_empty()
            || namespace.split('.').any(str::is_empty)
        {
            let message = format!("`{namespace}` is not a valid settings namespace");
            tracing::error!("{message}");
            self.registration
                .push(SettingsError::error(None, namespace, message));
            return false;
        }
        match make_entry::<T>() {
            Ok(entry) => {
                self.values.insert(namespace, entry.initial.clone());
                self.registry.insert(namespace, entry);
                true
            }
            Err(message) => {
                let message = format!(
                    "cannot register `{}`: {message}",
                    std::any::type_name::<T>()
                );
                tracing::error!("{message}");
                self.registration
                    .push(SettingsError::error(None, namespace, message));
                false
            }
        }
    }

    /// Rebuilds effective values and derived diagnostics from the layers. Returns the namespaces
    /// whose effective value changed.
    fn recompute(&mut self) -> BTreeSet<String> {
        let mut changed = BTreeSet::new();
        let mut derived = Vec::new();

        let namespaces: Vec<&'static str> = self.registry.keys().copied().collect();
        self.coverage(&self.user, false, &mut derived);
        self.coverage(&self.workspace, true, &mut derived);

        for namespace in namespaces {
            let Some(entry) = self.registry.get(namespace) else {
                continue;
            };
            let mut layers: Vec<(Option<&Path>, &Value)> = Vec::new();
            if let Some(v) = subtree(&self.user.table, namespace) {
                layers.push((self.user.path.as_deref(), v));
            }
            if entry.overridable
                && let Some(v) = subtree(&self.workspace.table, namespace)
            {
                layers.push((self.workspace.path.as_deref(), v));
            }
            if let Some(v) = self.runtime.get(namespace) {
                layers.push((None, v));
            }

            let mut layers_ok = true;
            let mut merged = entry.default.clone();
            for (file, layer) in &layers {
                let mut alone = entry.default.clone();
                deep_merge(&mut alone, layer);
                match (entry.load)(&alone) {
                    Ok(loaded) => {
                        // Unknown keys are only meaningful for files; runtime values are typed.
                        if file.is_some() {
                            derived.extend(loaded.unknown.iter().map(|path| {
                                SettingsError::warning(
                                    *file,
                                    join(namespace, path),
                                    "unknown key (ignored)",
                                )
                            }));
                        }
                    }
                    Err(message) => {
                        layers_ok = false;
                        derived.extend(attribute(entry, namespace, *file, layer, &message));
                    }
                }
                deep_merge(&mut merged, layer);
            }
            if !layers_ok {
                continue;
            }
            match (entry.load)(&merged) {
                Ok(loaded) => {
                    let differs = self
                        .values
                        .get(namespace)
                        .is_none_or(|prev| prev.json != loaded.computed.json);
                    if differs {
                        changed.insert(namespace.to_owned());
                        self.values.insert(namespace, loaded.computed);
                    }
                }
                Err(message) => {
                    derived.push(SettingsError::error(
                        None,
                        namespace,
                        format!("invalid combined value: {message}; keeping the last good value"),
                    ));
                }
            }
        }
        self.derived = derived;
        changed
    }

    /// Keys of `layer` that no registered namespace claims, and (workspace) keys of namespaces
    /// that may not be overridden per workspace.
    fn coverage(&self, layer: &Layer, workspace: bool, out: &mut Vec<SettingsError>) {
        self.walk("", &layer.table, layer.path.as_deref(), workspace, out);
    }

    fn walk(
        &self,
        prefix: &str,
        table: &toml::Table,
        file: Option<&Path>,
        workspace: bool,
        out: &mut Vec<SettingsError>,
    ) {
        for (key, value) in table {
            let path = join(prefix, key);
            if let Some(entry) = self.registry.get(path.as_str()) {
                if workspace && !entry.overridable {
                    out.push(SettingsError::error(
                        file,
                        path.clone(),
                        format!("`{path}` cannot be overridden per workspace; ignored"),
                    ));
                }
            } else if self.registry.keys().any(|ns| {
                ns.strip_prefix(path.as_str())
                    .is_some_and(|r| r.starts_with('.'))
            }) {
                match value {
                    Value::Table(inner) => self.walk(&path, inner, file, workspace, out),
                    _ => out.push(SettingsError::error(file, path, "expected a table")),
                }
            } else {
                out.push(SettingsError::warning(
                    file,
                    path,
                    "unknown settings namespace (ignored)",
                ));
            }
        }
    }
}

/// Pins a deserialization failure of `layer` on the offending top-level keys when possible.
fn attribute(
    entry: &Entry,
    namespace: &str,
    file: Option<&Path>,
    layer: &Value,
    message: &str,
) -> Vec<SettingsError> {
    let mut errors = Vec::new();
    if let Value::Table(table) = layer {
        for (key, value) in table {
            let mut alone = entry.default.clone();
            let mut single = toml::Table::new();
            single.insert(key.clone(), value.clone());
            deep_merge(&mut alone, &Value::Table(single));
            if let Err(message) = (entry.load)(&alone) {
                errors.push(SettingsError::error(
                    file,
                    join(namespace, key),
                    format!("{message}; keeping the last good value"),
                ));
            }
        }
    }
    if errors.is_empty() {
        errors.push(SettingsError::error(
            file,
            namespace,
            format!("{message}; keeping the last good value"),
        ));
    }
    errors
}

pub(crate) struct Shared {
    state: RwLock<State>,
    tx: watch::Sender<ChangeSet>,
    pub(crate) watch: Mutex<Option<WatchState>>,
}

impl Shared {
    fn commit(&self, state: &mut State, mut changed: BTreeSet<String>, keymap_changed: bool) {
        if keymap_changed {
            changed.insert(KEYMAP_NAMESPACE.to_owned());
        }
        let errors = state.errors();
        let errors_changed = errors != state.published_errors;
        state.published_errors = errors;
        if changed.is_empty() && !errors_changed {
            return;
        }
        state.generation += 1;
        self.tx.send_replace(ChangeSet {
            generation: state.generation,
            namespaces: changed,
        });
    }

    /// Re-reads all three files and publishes whatever changed. Called by the watcher thread.
    pub(crate) fn reload_from_disk(&self) {
        let mut state = self.state.write();
        state.user.refresh(Kind::User);
        let keymap_changed = state.keymap.refresh(Kind::Keymap);
        state.workspace.refresh(Kind::Workspace);
        let changed = state.recompute();
        self.commit(&mut state, changed, keymap_changed);
    }

    /// Directories that must be watched for the current files to hot-reload.
    pub(crate) fn watch_dirs(&self) -> BTreeSet<PathBuf> {
        let state = self.state.read();
        let mut dirs = BTreeSet::new();
        for layer in [&state.user, &state.keymap] {
            if let Some(parent) = layer.path.as_deref().and_then(Path::parent) {
                dirs.insert(parent.to_path_buf());
            }
        }
        if let Some(root) = &state.workspace_root {
            let dot = root.join(".heeler");
            // `.heeler` may not exist yet; watch the root until it does (watch handler refilters).
            dirs.insert(if dot.is_dir() { dot } else { root.clone() });
        }
        dirs
    }

    pub(crate) fn user_dir(&self) -> Option<PathBuf> {
        self.state
            .read()
            .user
            .path
            .as_deref()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
    }
}

/// Layered, validated, hot-reloadable settings. Cheap to clone; clones share state.
#[derive(Clone)]
pub struct SettingsStore {
    shared: Arc<Shared>,
}

impl fmt::Debug for SettingsStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SettingsStore").finish_non_exhaustive()
    }
}

impl SettingsStore {
    /// Loads `settings.toml` and `keymap.toml` once. Problems are recorded in [`errors`](Self::errors),
    /// never returned: a store always starts.
    pub fn new(paths: SettingsPaths) -> Self {
        let mut state = State {
            user: Layer::at(Some(paths.user)),
            keymap: Layer::at(Some(paths.keymap)),
            workspace: Layer::at(None),
            workspace_root: None,
            registry: BTreeMap::new(),
            runtime: BTreeMap::new(),
            values: BTreeMap::new(),
            derived: Vec::new(),
            registration: Vec::new(),
            published_errors: Vec::new(),
            generation: 0,
        };
        state.user.refresh(Kind::User);
        state.keymap.refresh(Kind::Keymap);
        state.published_errors = state.errors();
        let (tx, _) = watch::channel(ChangeSet::default());
        Self {
            shared: Arc::new(Shared {
                state: RwLock::new(state),
                tx,
                watch: Mutex::new(None),
            }),
        }
    }

    /// Declares a namespace: it takes part in merging, validation, unknown-key checks and
    /// [`schema`](Self::schema). Registering the same type twice is a no-op. `get` and
    /// `set_runtime` register implicitly, so registering early only matters for diagnostics and
    /// the schema.
    pub fn register<T: Setting>(&self) {
        let mut state = self.shared.state.write();
        if state.register::<T>() {
            let mut changed = state.recompute();
            changed.remove(T::NAMESPACE); // first value of a new namespace is not a change
            self.shared.commit(&mut state, changed, false);
        }
    }

    /// The merged, validated value of `T` (last good value when the files are currently invalid;
    /// `T::default()` when nothing configures it).
    pub fn get<T: Setting>(&self) -> Arc<T> {
        if let Some(value) = self.lookup::<T>() {
            return value;
        }
        self.register::<T>();
        self.lookup::<T>().unwrap_or_default()
    }

    fn lookup<T: Setting>(&self) -> Option<Arc<T>> {
        let state = self.shared.state.read();
        let entry = state.registry.get(T::NAMESPACE)?;
        if entry.type_id != TypeId::of::<T>() {
            return None;
        }
        state
            .values
            .get(T::NAMESPACE)?
            .value
            .clone()
            .downcast::<T>()
            .ok()
    }

    /// Uses `<root>/.heeler/settings.toml` as the workspace layer (`None` removes it).
    pub fn set_workspace_root(&self, root: Option<PathBuf>) {
        {
            let mut state = self.shared.state.write();
            state.workspace = Layer::at(root.as_deref().map(workspace_file));
            state.workspace_root = root;
            state.workspace.refresh(Kind::Workspace);
            let changed = state.recompute();
            self.shared.commit(&mut state, changed, false);
        }
        if crate::watch::rewatch(&self.shared) {
            self.shared.reload_from_disk();
        }
    }

    /// Sets the in-memory override layer for `T`'s namespace. It sits above every file layer and
    /// is never written to disk. Registers `T` if needed.
    pub fn set_runtime<T: Setting>(&self, value: T) {
        let mut state = self.shared.state.write();
        state.register::<T>();
        let Some(runtime) = serde_json::to_value(&value)
            .ok()
            .as_ref()
            .and_then(json_to_toml)
        else {
            let message = format!(
                "runtime value of `{}` is not representable as TOML",
                T::NAMESPACE
            );
            tracing::error!("{message}");
            state
                .registration
                .push(SettingsError::error(None, T::NAMESPACE, message));
            let changed = BTreeSet::new();
            self.shared.commit(&mut state, changed, false);
            return;
        };
        state.runtime.insert(T::NAMESPACE, runtime);
        let changed = state.recompute();
        self.shared.commit(&mut state, changed, false);
    }

    /// Sets `key_path` (dotted, e.g. `terminal.font_size`) in `settings.toml`, creating the file
    /// and its directory (`0700`) when missing. Comments and formatting elsewhere survive. The
    /// change is applied to the store immediately; the file watcher later sees identical text.
    pub fn write_user(&self, key_path: &str, value: Value) -> Result<(), SettingsError> {
        let mut state = self.shared.state.write();
        let path = state
            .user
            .path
            .clone()
            .ok_or_else(|| SettingsError::error(None, key_path, "no user settings path"))?;
        let fail = |message: String| SettingsError::error(Some(&path), key_path, message);

        let existing = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(fail(format!("cannot read file: {e}"))),
        };
        let updated = edit::set_key(&existing, key_path, &value).map_err(fail)?;
        write_atomic(&path, &updated).map_err(|e| fail(format!("cannot write file: {e}")))?;

        state.user.refresh(Kind::User);
        let changed = state.recompute();
        self.shared.commit(&mut state, changed, false);
        Ok(())
    }

    /// Receiver for update notifications; see [`ChangeSet`].
    pub fn changes(&self) -> watch::Receiver<ChangeSet> {
        self.shared.tx.subscribe()
    }

    /// Every current problem: parse errors, invalid values, refused workspace keys (errors) and
    /// unknown keys / keymap conflicts (warnings).
    pub fn errors(&self) -> Vec<SettingsError> {
        self.shared.state.read().errors()
    }

    /// JSON description of every registered namespace for the settings UI:
    /// `{ "namespaces": { "<ns>": { "workspace_overridable": bool, "schema": <JSON Schema> } } }`.
    pub fn schema(&self) -> Json {
        let state = self.shared.state.read();
        let namespaces: serde_json::Map<String, Json> = state
            .registry
            .iter()
            .map(|(namespace, entry)| {
                (
                    (*namespace).to_owned(),
                    serde_json::json!({
                        "workspace_overridable": entry.overridable,
                        "schema": entry.schema,
                    }),
                )
            })
            .collect();
        serde_json::json!({ "namespaces": namespaces })
    }

    /// The effective keymap: `defaults` merged with the last good `keymap.toml`. Diagnostics
    /// include conflicts against `defaults`.
    pub fn keymap(&self, defaults: &[keymap::BindingSpec]) -> keymap::KeymapLoad {
        let state = self.shared.state.read();
        let text = state.keymap.text.as_deref().unwrap_or("");
        match keymap::load_str(defaults, text, state.keymap.path.as_deref()) {
            Ok(load) => load,
            // The text was validated when it was accepted; defaults alone are the safe answer.
            Err(error) => keymap::KeymapLoad {
                bindings: keymap::load_str(defaults, "", None)
                    .map(|l| l.bindings)
                    .unwrap_or_default(),
                diagnostics: vec![error],
            },
        }
    }

    /// Hot-reloads `settings.toml`, `keymap.toml` and the workspace file until the guard drops.
    /// Creates the settings directory (`0700`) if missing so it can be watched.
    pub fn start_watching(&self) -> Result<WatchGuard, SettingsError> {
        crate::watch::start(&self.shared)
    }
}

pub(crate) fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
}

/// Write-then-rename in the target's directory; follows a symlinked `settings.toml` (dotfile
/// managers) and keeps the existing file's permissions.
fn write_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    create_private_dir(dir)?;
    let mut tmp_name = target.file_name().unwrap_or_default().to_os_string();
    tmp_name.push(format!(".tmp-{}", std::process::id()));
    let tmp = dir.join(tmp_name);
    let result = (|| {
        std::fs::write(&tmp, text)?;
        if let Ok(meta) = std::fs::metadata(&target) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}
