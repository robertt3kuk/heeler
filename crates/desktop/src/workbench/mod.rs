//! The workbench: GPUI-free layout model plus the GPUI shell around it.

pub mod actions;
pub(crate) mod agents;
mod assistant;
pub mod chat;
mod daemon;
mod daemon_popover;
pub(crate) mod harnesses;
mod history;
mod mac;
mod modal;
pub mod model;
mod monitor;
mod notify;
pub mod palette;
pub mod panels;
pub mod persistence;
mod providers;
pub mod registry;
mod render;
pub mod settings_schema;
mod settings_sheet;
pub mod style;
mod terminals;
pub mod ui;
mod view;
mod workspaces;
mod worktrees;

pub use agents::{AgentPicker, CODEX, Models};
pub use daemon::{DaemonBusy, log_file as daemon_log_file};
pub use harnesses::{Detection, HARNESSES, Harness, HarnessDirectory, Listing};
pub use modal::{Modal, ModalCheckbox, ModalChoice};
pub use panels::{HistoryConfirm, HistoryPanel, StatusTone, connection_label};
pub use registry::{PanelEnv, StatusBarLayout, StatusData};
pub use settings_sheet::{Edit, SettingsSheet};
pub use terminals::session_of_item;
pub use view::{
    BadgeSite, DragPreview, DraggedItem, Region, SAVE_DEBOUNCE, Workbench, WorkbenchOptions,
    preset_label,
};
pub use workspaces::{HistoryState, PickerEntry, PickerKind, PickerState};
