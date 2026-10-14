//! Panel and status-item registries (ADR 0010: every UI feature is a registered component).

use super::model::{DockPosition, PanelId};
use crate::theme::{Density, Theme};
use gpui::{AnyElement, AnyView, App, SharedString, Window};
use std::rc::Rc;

/// Builds the view of a panel. Called once per workbench; the view is cached and re-rendered.
pub type PanelFactory = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;

#[derive(Clone)]
pub struct PanelSpec {
    pub id: PanelId,
    pub title: SharedString,
    /// A single glyph; no icon assets are bundled yet.
    pub icon: SharedString,
    pub default_dock: DockPosition,
    pub factory: PanelFactory,
}

impl std::fmt::Debug for PanelSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PanelSpec")
            .field("id", &self.id)
            .field("title", &self.title)
            .field("default_dock", &self.default_dock)
            .finish_non_exhaustive()
    }
}

/// id → panel spec, in registration order.
#[derive(Clone, Debug, Default)]
pub struct PanelRegistry {
    specs: Vec<PanelSpec>,
}

impl PanelRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a panel; a later registration with the same id replaces the earlier one.
    pub fn register(&mut self, spec: PanelSpec) {
        match self.specs.iter_mut().find(|s| s.id == spec.id) {
            Some(existing) => *existing = spec,
            None => self.specs.push(spec),
        }
    }

    pub fn get(&self, id: &PanelId) -> Option<&PanelSpec> {
        self.specs.iter().find(|s| s.id == *id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &PanelSpec> {
        self.specs.iter()
    }

    /// Title for `id`, falling back to the id itself for unregistered (e.g. removed) panels.
    pub fn title(&self, id: &PanelId) -> SharedString {
        self.get(id)
            .map(|s| s.title.clone())
            .unwrap_or_else(|| SharedString::from(id.to_string()))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusSide {
    Left,
    Right,
}

/// Everything a status item needs to draw itself.
#[derive(Debug)]
pub struct StatusContext<'a> {
    pub theme: &'a Theme,
    pub density: Density,
    /// Key hint of the palette action ("⌘K"), from the live keymap.
    pub palette_hint: SharedString,
}

pub type StatusRender = Rc<dyn Fn(&StatusContext<'_>) -> AnyElement>;

#[derive(Clone)]
pub struct StatusItemSpec {
    pub id: &'static str,
    pub default_side: StatusSide,
    pub render: StatusRender,
}

impl std::fmt::Debug for StatusItemSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatusItemSpec")
            .field("id", &self.id)
            .field("default_side", &self.default_side)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct StatusItemRegistry {
    specs: Vec<StatusItemSpec>,
}

impl StatusItemRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, spec: StatusItemSpec) {
        match self.specs.iter_mut().find(|s| s.id == spec.id) {
            Some(existing) => *existing = spec,
            None => self.specs.push(spec),
        }
    }

    pub fn get(&self, id: &str) -> Option<&StatusItemSpec> {
        self.specs.iter().find(|s| s.id == id)
    }

    pub fn iter(&self) -> impl Iterator<Item = &StatusItemSpec> {
        self.specs.iter()
    }
}

/// Ordered status item ids per side (`layout.presets.<name>.status_bar` in design.md). Unknown ids
/// are skipped at render time so a layout can reference items that are not registered yet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusBarLayout {
    pub left: Vec<String>,
    pub right: Vec<String>,
}

impl Default for StatusBarLayout {
    fn default() -> Self {
        Self {
            left: vec!["connection".to_owned()],
            right: vec!["density".to_owned(), "palette_hint".to_owned()],
        }
    }
}
