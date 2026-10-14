//! Versioned JSON (de)serialization of [`WorkbenchState`].

use super::{ModelError, WorkbenchState};

pub const STATE_VERSION: u32 = 1;

/// Result of reading a saved state.
#[derive(Debug)]
pub struct LoadOutcome {
    pub state: WorkbenchState,
    /// Set when the saved data was unusable and the default layout was substituted.
    pub warning: Option<String>,
}

impl WorkbenchState {
    /// `{ "version": 1, "docks": …, "center": …, … }`, pretty-printed.
    pub fn to_json(&self) -> Result<String, ModelError> {
        let mut value =
            serde_json::to_value(self).map_err(|e| ModelError::Invalid(e.to_string()))?;
        if let Some(map) = value.as_object_mut() {
            map.insert("version".to_owned(), STATE_VERSION.into());
        }
        serde_json::to_string_pretty(&value).map_err(|e| ModelError::Invalid(e.to_string()))
    }

    /// Strict parse: wrong version or malformed data is an error. The result is normalized, so
    /// hand-edited files with broken ratios or empty panes still load into a valid state.
    pub fn from_json(json: &str) -> Result<Self, ModelError> {
        let value: serde_json::Value =
            serde_json::from_str(json).map_err(|e| ModelError::Invalid(e.to_string()))?;
        match value.get("version").and_then(serde_json::Value::as_u64) {
            Some(v) if v == u64::from(STATE_VERSION) => {}
            Some(v) => return Err(ModelError::Invalid(format!("unsupported version {v}"))),
            None => return Err(ModelError::Invalid("missing version".to_owned())),
        }
        let mut state: WorkbenchState =
            serde_json::from_value(value).map_err(|e| ModelError::Invalid(e.to_string()))?;
        state.normalize();
        Ok(state)
    }

    /// Like [`Self::from_json`] but falls back to the default layout with a warning.
    pub fn from_json_or_default(json: &str) -> LoadOutcome {
        match Self::from_json(json) {
            Ok(state) => LoadOutcome {
                state,
                warning: None,
            },
            Err(err) => LoadOutcome {
                state: WorkbenchState::default(),
                warning: Some(format!("saved layout ignored: {err}")),
            },
        }
    }
}
