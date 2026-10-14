//! Layout presets and applying them without losing open items.

use super::{
    Dock, DockPosition, Docks, FocusTarget, ItemRef, ModelError, Pane, PaneTree, PanelId,
    SavedLayout, WorkbenchState,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// What a preset does to the center.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CenterLayout {
    /// Leave the pane tree as it is.
    Keep,
    /// Replace the tree with one pane showing these items first.
    Items(Vec<ItemRef>),
}

/// A named arrangement of docks and (optionally) the center.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LayoutPreset {
    pub name: String,
    pub docks: Docks,
    pub center: CenterLayout,
    /// Zoom the active pane after applying (the `focus` preset).
    pub zoom_active: bool,
}

impl LayoutPreset {
    pub const DEFAULT: &'static str = "default";
    pub const FOCUS: &'static str = "focus";
    pub const OVERVIEW: &'static str = "overview";

    fn docks(left: Dock, right: Dock) -> Docks {
        Docks {
            left,
            right,
            bottom: Dock::new(&[], 200.0, false),
        }
    }

    /// `sessions` left, `inspector` right, bottom hidden, center untouched.
    pub fn default_preset() -> Self {
        Self {
            name: Self::DEFAULT.to_owned(),
            docks: Self::docks(
                Dock::new(&["sessions"], 240.0, true),
                Dock::new(&["inspector"], 300.0, true),
            ),
            center: CenterLayout::Keep,
            zoom_active: false,
        }
    }

    /// All docks hidden, the active pane zoomed.
    pub fn focus_preset() -> Self {
        Self {
            name: Self::FOCUS.to_owned(),
            docks: Self::docks(
                Dock::new(&["sessions"], 240.0, false),
                Dock::new(&["inspector"], 300.0, false),
            ),
            center: CenterLayout::Keep,
            zoom_active: true,
        }
    }

    /// `overview` panel as the first center item, side docks visible.
    pub fn overview_preset() -> Self {
        Self {
            name: Self::OVERVIEW.to_owned(),
            docks: Self::docks(
                Dock::new(&["sessions"], 240.0, true),
                Dock::new(&["inspector"], 300.0, true),
            ),
            center: CenterLayout::Items(vec![ItemRef::panel("overview")]),
            zoom_active: false,
        }
    }

    pub fn builtin() -> Vec<LayoutPreset> {
        vec![
            Self::default_preset(),
            Self::focus_preset(),
            Self::overview_preset(),
        ]
    }

    /// Panels this preset places (docks and center override).
    fn placed_panels(&self) -> HashSet<PanelId> {
        let mut placed: HashSet<PanelId> = DockPosition::ALL
            .into_iter()
            .flat_map(|pos| self.docks.get(pos).panels.iter().cloned())
            .collect();
        if let CenterLayout::Items(items) = &self.center {
            placed.extend(items.iter().filter_map(|i| i.as_panel().cloned()));
        }
        placed
    }
}

impl WorkbenchState {
    /// Apply `preset`, keeping every open item.
    ///
    /// Docks are replaced by the preset's. Panels the preset places are moved to those places;
    /// items it does not place (terminals, chats, placeholders, panels that were open elsewhere)
    /// are kept in the first pane. A [`CenterLayout::Items`] override flattens the old tree into
    /// one pane (override items first).
    pub fn apply_preset(&mut self, preset: &LayoutPreset) {
        let placed = preset.placed_panels();
        // A center override (overview) remembers the layout it replaced; re-applying while one
        // is already remembered keeps the original, and any other preset forgets it.
        let stash = match &preset.center {
            CenterLayout::Items(_) => Some(
                self.stashed
                    .take()
                    .unwrap_or_else(|| Box::new(SavedLayout::of(self))),
            ),
            CenterLayout::Keep => None,
        };
        let prev_focus = self.active_pane_id();
        let old_docks = std::mem::replace(&mut self.docks, preset.docks.clone());
        let leftover_panels: Vec<ItemRef> = DockPosition::ALL
            .into_iter()
            .flat_map(|pos| old_docks.get(pos).panels.iter().cloned())
            .filter(|p| !placed.contains(p))
            .map(|panel| ItemRef::Panel { panel })
            .collect();

        match &preset.center {
            CenterLayout::Keep => {
                for pane in self.center.panes_mut() {
                    pane.items
                        .retain(|i| i.as_panel().is_none_or(|p| !placed.contains(p)));
                    pane.active = pane.active.min(pane.items.len().saturating_sub(1));
                }
                if let Some(first) = self.center.first_pane_mut() {
                    first.items.extend(leftover_panels);
                }
            }
            CenterLayout::Items(items) => {
                let first_id = self.first_pane_id();
                let mut all: Vec<ItemRef> = items.clone();
                all.extend(leftover_panels);
                for pane in self.center.panes() {
                    all.extend(
                        pane.items
                            .iter()
                            .filter(|i| i.as_panel().is_none_or(|p| !placed.contains(p)))
                            .cloned(),
                    );
                }
                self.center = PaneTree::single(Pane::new(first_id, all));
            }
        }

        if self.preset != preset.name {
            self.previous_preset = Some(std::mem::replace(&mut self.preset, preset.name.clone()));
        }
        self.zoomed = None;
        self.focus = FocusTarget::Pane(prev_focus);
        self.stashed = stash;
        self.normalize();
        // `normalize` moved focus to the first pane if the focused one no longer exists.
        if preset.zoom_active {
            self.zoomed = Some(self.active_pane_id());
        }
    }

    /// Put `saved` back exactly, keeping only items that are still open and adding items opened
    /// since (other than panels the departing preset placed itself) to the first pane.
    fn restore_saved(&mut self, saved: SavedLayout, departing_panels: &HashSet<PanelId>) {
        let mut live: HashMap<ItemRef, usize> = HashMap::new();
        for item in self.all_items() {
            *live.entry(item).or_insert(0) += 1;
        }
        let mut take = |item: &ItemRef| match live.get_mut(item) {
            Some(count) if *count > 0 => {
                *count -= 1;
                true
            }
            _ => false,
        };
        let mut restored = saved.into_state(self.next_pane);
        for pos in DockPosition::ALL {
            restored
                .docks
                .get_mut(pos)
                .panels
                .retain(|p| take(&ItemRef::Panel { panel: p.clone() }));
        }
        for pane in restored.center.panes_mut() {
            pane.items.retain(|item| take(item));
        }
        let mut fresh = Vec::new();
        for item in self.all_items() {
            if let Some(count) = live.get_mut(&item)
                && *count > 0
            {
                *count -= 1;
                if item
                    .as_panel()
                    .is_none_or(|p| !departing_panels.contains(p))
                {
                    fresh.push(item);
                }
            }
        }
        // `normalize` below prunes panes the filtering emptied; give them a home first.
        if let Some(first) = restored.center.first_pane_mut() {
            first.items.extend(fresh);
        }
        restored.normalize();
        *self = restored;
    }

    /// Apply the preset called `name`, or if it is already the current preset, go back to the
    /// previous one (or `default`). Leaving a preset that put panels in the center removes the
    /// ones the new preset does not place (the overview board only lives while selected).
    pub fn toggle_preset(
        &mut self,
        name: &str,
        presets: &[LayoutPreset],
    ) -> Result<(), ModelError> {
        let find = |n: &str| presets.iter().find(|p| p.name == n);
        if self.preset != name {
            let target = find(name).ok_or_else(|| ModelError::UnknownPreset(name.to_owned()))?;
            self.apply_preset(target);
            return Ok(());
        }
        let departing = find(name).ok_or_else(|| ModelError::UnknownPreset(name.to_owned()))?;
        if let Some(saved) = self.stashed.take() {
            self.restore_saved(*saved, &departing.placed_panels());
            return Ok(());
        }
        let back_name = self
            .previous_preset
            .clone()
            .filter(|p| p != name)
            .unwrap_or_else(|| LayoutPreset::DEFAULT.to_owned());
        let back = find(&back_name).ok_or(ModelError::UnknownPreset(back_name))?;
        self.apply_preset(back);
        if let CenterLayout::Items(items) = &departing.center {
            for panel in items.iter().filter_map(ItemRef::as_panel) {
                if matches!(self.find_panel(panel), Some(super::ItemSource::Pane { .. })) {
                    self.close_panel(panel)?;
                }
            }
        }
        Ok(())
    }
}
