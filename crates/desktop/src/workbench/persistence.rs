//! `desktop-state.json` location, load, and atomic save.

use super::model::{ModelError, WorkbenchState};
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub const STATE_FILE: &str = "desktop-state.json";

/// `$HEELER_HOME/desktop-state.json`, else
/// `~/Library/Application Support/Heeler/desktop-state.json`. `None` when neither a Heeler home
/// nor a user home is known.
pub fn state_path_from(
    heeler_home: Option<OsString>,
    user_home: Option<OsString>,
) -> Option<PathBuf> {
    match heeler_home.filter(|h| !h.is_empty()) {
        Some(home) => Some(PathBuf::from(home).join(STATE_FILE)),
        None => user_home.filter(|h| !h.is_empty()).map(|home| {
            PathBuf::from(home)
                .join("Library/Application Support/Heeler")
                .join(STATE_FILE)
        }),
    }
}

/// [`state_path_from`] over the process environment.
pub fn default_state_path() -> Option<PathBuf> {
    state_path_from(std::env::var_os("HEELER_HOME"), std::env::var_os("HOME"))
}

/// Read the saved state. Missing file → default layout; corrupt or unsupported → default layout
/// plus a logged warning.
pub fn load_state(path: &Path) -> WorkbenchState {
    match std::fs::read_to_string(path) {
        Ok(json) => {
            let outcome = WorkbenchState::from_json_or_default(&json);
            match outcome.warning {
                Some(warning) => {
                    tracing::warn!(path = %path.display(), "{warning}; using the default layout")
                }
                None => tracing::info!(path = %path.display(), "restored desktop layout"),
            }
            outcome.state
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            tracing::info!(path = %path.display(), "no saved desktop layout; using the default layout");
            WorkbenchState::default()
        }
        Err(err) => {
            tracing::warn!(path = %path.display(), "cannot read desktop layout: {err}; using the default layout");
            WorkbenchState::default()
        }
    }
}

/// Blocks whose output a terminal view folds, per terminal (terminal id → block ids). View state
/// like the layout, saved beside it (`collapsed_blocks` in the state file) so folds survive a
/// restart of the desktop while the daemon keeps the terminal; never sent to the daemon.
pub type CollapsedBlocks = BTreeMap<String, Vec<u64>>;

/// The folded blocks saved in `path`; none when the file is missing or has none.
pub fn load_collapsed(path: &Path) -> CollapsedBlocks {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|json| serde_json::from_str::<serde_json::Value>(&json).ok())
        .and_then(|mut value| {
            value
                .get_mut("collapsed_blocks")
                .map(serde_json::Value::take)
        })
        .and_then(|value| serde_json::from_value(value).ok())
        .unwrap_or_default()
}

/// The state file's JSON: the layout plus, when there are any, the folded blocks.
pub fn state_json(
    state: &WorkbenchState,
    collapsed: &CollapsedBlocks,
) -> Result<String, ModelError> {
    let json = state.to_json()?;
    if collapsed.is_empty() {
        return Ok(json);
    }
    let invalid = |err: serde_json::Error| ModelError::Invalid(err.to_string());
    let mut value: serde_json::Value = serde_json::from_str(&json).map_err(invalid)?;
    if let Some(map) = value.as_object_mut() {
        map.insert(
            "collapsed_blocks".to_owned(),
            serde_json::to_value(collapsed).map_err(invalid)?,
        );
    }
    serde_json::to_string_pretty(&value).map_err(invalid)
}

/// Write `json` next to `path` and rename it into place so a crash never leaves a torn file.
pub fn save_json(path: &Path, json: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heeler_home_wins() {
        let path = state_path_from(Some("/tmp/hh".into()), Some("/Users/x".into()));
        assert_eq!(path, Some(PathBuf::from("/tmp/hh/desktop-state.json")));
    }

    #[test]
    fn falls_back_to_application_support() {
        let path = state_path_from(None, Some("/Users/x".into()));
        assert_eq!(
            path,
            Some(PathBuf::from(
                "/Users/x/Library/Application Support/Heeler/desktop-state.json"
            ))
        );
        assert_eq!(
            state_path_from(Some("".into()), Some("/Users/x".into())),
            path
        );
        assert_eq!(state_path_from(None, None), None);
    }

    #[test]
    fn folded_blocks_travel_in_the_state_file_beside_the_layout() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATE_FILE);
        let state = WorkbenchState::default();
        // Without folds the file is exactly the layout.
        assert_eq!(
            state_json(&state, &CollapsedBlocks::new()).unwrap(),
            state.to_json().unwrap()
        );
        let mut collapsed = CollapsedBlocks::new();
        collapsed.insert("01TERMINAL".to_owned(), vec![3, 7]);
        save_json(&path, &state_json(&state, &collapsed).unwrap()).unwrap();
        assert_eq!(load_collapsed(&path), collapsed);
        // The layout still loads from the same file.
        assert_eq!(load_state(&path), state);
        // Missing and corrupt files hold none.
        assert!(load_collapsed(&dir.path().join("none.json")).is_empty());
        std::fs::write(&path, "{ not json").unwrap();
        assert!(load_collapsed(&path).is_empty());
    }

    #[test]
    fn missing_corrupt_and_good_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join(STATE_FILE);
        assert_eq!(load_state(&path), WorkbenchState::default());

        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{ nope").unwrap();
        assert_eq!(load_state(&path), WorkbenchState::default());

        let mut state = WorkbenchState::default();
        state.toggle_dock(super::super::model::DockPosition::Left);
        save_json(&path, &state.to_json().unwrap()).unwrap();
        assert_eq!(load_state(&path), state);
        assert!(!path.with_extension("json.tmp").exists());
    }
}
