//! The workbench: GPUI-free layout model plus the GPUI shell around it.

pub mod actions;
pub mod model;
pub mod palette;
pub mod panels;
pub mod persistence;
pub mod registry;
mod render;
pub mod style;
mod view;

pub use view::{
    DragPreview, DraggedItem, Region, SAVE_DEBOUNCE, Workbench, WorkbenchOptions, preset_label,
};
