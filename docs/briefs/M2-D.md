# M2-D: `settings` — layered TOML settings, schemas, hot reload, keymap, themes, density (roadmap M2.4 engine)

Agent: implementer   Depends on: master   Branch: slice/M2-D

## Context
Read `AGENTS.md`, ADR 0010 (§ Settings), `docs/design.md` (§ Settings, § Visual system: density tokens and terminal themes, § Keyboard defaults).

## Scope
New crate `crates/settings` (package `staffy-settings`, library). **No GPUI.** Allowed deps: `serde`, `toml`, `toml_edit` (comment-preserving writes), `schemars`, `notify` (+ a debouncer), `thiserror`, `tracing`, `parking_lot`/std sync, `tokio` only for `sync::watch`. Do not edit other crates.

## Contract
```rust
pub trait Setting: Serialize + DeserializeOwned + JsonSchema + Default + Clone + Send + Sync + 'static {
    const NAMESPACE: &'static str;            // dotted path, e.g. "terminal", "panels.sessions", "layout"
    const WORKSPACE_OVERRIDABLE: bool = false;
}

pub struct SettingsPaths { pub user: PathBuf, pub keymap: PathBuf }   // from STAFFY_HOME or ~/Library/Application Support/Staffy
pub struct SettingsStore { /* Send + Sync */ }
impl SettingsStore {
    pub fn new(paths: SettingsPaths) -> Self;
    pub fn register<T: Setting>(&self);
    pub fn get<T: Setting>(&self) -> Arc<T>;                       // merged + validated; defaults if absent
    pub fn set_workspace_root(&self, root: Option<PathBuf>);       // loads <root>/.staffy/settings.toml
    pub fn set_runtime<T: Setting>(&self, value: T);               // highest layer, in memory
    pub fn write_user(&self, key_path: &str, value: toml::Value) -> Result<(), SettingsError>; // edits settings.toml in place, preserving comments/formatting
    pub fn changes(&self) -> tokio::sync::watch::Receiver<ChangeSet>; // generation + namespaces changed
    pub fn errors(&self) -> Vec<SettingsError>;                    // file, key path, message
    pub fn schema(&self) -> serde_json::Value;                     // all registered namespaces, for the settings UI
    pub fn start_watching(&self) -> Result<WatchGuard, SettingsError>; // hot reload user, keymap, workspace files
}
```
Semantics:
- Layers: defaults < user < workspace (only for namespaces with `WORKSPACE_OVERRIDABLE`; other keys in the workspace file produce an error entry and are ignored) < runtime. Tables merge deeply; arrays and scalars replace.
- Per namespace: deserialize the merged table into `T` with `#[serde(default)]` semantics; on error keep the last good value for that namespace and record an error; unknown keys produce warnings (not failures).
- Reload on file change (debounced ~100 ms); a parse error in a file keeps the previous good layer for that file.
- `write_user` creates the file (and parent dir `0700`) if missing; preserves comments and unrelated formatting.
- **Keymap** (`keymap::load`): `[[binding]] keys = "cmd-shift-d", action = "workbench::SplitDown", context = "Pane"`; `action = "none"` unbinds a default. Parse/validate key syntax (cmd, ctrl, alt/option, shift, fn + key names); merge with defaults supplied by the caller; report conflicts (same keys + context) as warnings. GPUI-free output: `Vec<BindingSpec { keys: Vec<Keystroke>, action: String, context: Option<String> }>`.
- **Appearance**: `AppearanceSettings { density: Density /* compact|comfortable */, chrome: ChromeMode /* system|light|dark */ }` registered under `appearance`; `DensityTokens` table exactly as design.md (sidebar row, tab height, pane header, status bar, spacing, radius).
- **Terminal themes** (`themes`): `TerminalTheme { name, foreground, background, cursor, selection, ansi: [Rgb; 16] }`; built-ins `staffy-dark` (bg `#1e1e1e`, fg `#d4d4d4`, cursor `#d4d4d4`) and `staffy-light`; lookup by name; `auto` resolves by system appearance passed in by the caller.

## Acceptance
- Unit tests: layer precedence and deep merge; workspace override allowed vs refused; invalid value keeps last good + error; unknown key warning; `write_user` preserves comments (golden text); keymap parse, unbind, conflicts, invalid keys; theme lookup; schema contains registered namespaces.
- Watcher test with a temp dir: edit file → `changes()` fires with the namespace; broken edit → error recorded, values unchanged; fixed edit → recovered.
- `just check` green.

## Out of scope
Settings UI, registering desktop/terminal namespaces (wave 2), daemon settings.

## Report
Summary, files changed, commands run with results, deviations, open questions. Commit on `slice/M2-D` (scope `settings`).
