//! The workbench: GPUI-free layout model plus the GPUI shell around it.

pub mod actions;
mod modal;
pub mod model;
pub mod palette;
pub mod panels;
pub mod persistence;
pub mod registry;
mod render;
pub mod settings_schema;
mod settings_sheet;
pub mod style;
mod terminals;
mod view;

pub use modal::Modal;
pub use panels::connection_label;
pub use registry::{PanelEnv, StatusBarLayout, StatusData};
pub use settings_sheet::{Edit, SettingsSheet};
pub use terminals::session_of_item;
pub use view::{
    DragPreview, DraggedItem, Region, SAVE_DEBOUNCE, Workbench, WorkbenchOptions, preset_label,
};
