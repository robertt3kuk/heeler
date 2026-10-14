//! Named workbench actions, the command table behind the ⌘K palette, and the default keymap.
//!
//! One list feeds three consumers (ADR 0010): the palette, the menus, and the keymap. Wave 2
//! replaces [`default_keymap`] with `keymap.toml`; action names stay stable.

use super::model::{Direction, DockPosition, Edge, LayoutPreset, PanelDest};
use super::registry::PanelRegistry;
use gpui::{Action, KeyBinding, SharedString, actions};

actions!(
    workbench,
    [
        SplitRight,
        SplitDown,
        ToggleLeftDock,
        ToggleRightDock,
        ToggleBottomDock,
        ToggleOverview,
        ClosePaneItem,
        ToggleZoom,
        TogglePalette,
        NewTab,
        ResetLayout,
        OpenSettings,
        About,
        Quit,
        MinimizeWindow,
        ZoomWindow,
        ToggleDensity,
        SetDensityCompact,
        SetDensityComfortable,
        OpenActivePanelAsTab,
    ]
);

/// Palette-local actions, bound only inside the `Palette` key context.
pub mod palette {
    use gpui::actions;
    actions!(palette, [SelectNext, SelectPrev, Confirm, Dismiss]);
}

macro_rules! data_actions {
    ($($(#[$meta:meta])* $name:ident($ty:ty);)*) => {
        $(
            $(#[$meta])*
            #[derive(Clone, PartialEq, Debug, gpui::Action)]
            #[action(namespace = workbench, no_json)]
            pub struct $name(pub $ty);
        )*
    };
}

data_actions! {
    /// Apply the named layout preset (`default`, `focus`, `overview`, …).
    ApplyPreset(String);
    /// Reveal the panel with this id, opening it in its default dock if it is closed.
    OpenPanel(String);
    FocusNeighbor(Direction);
    ResizeFocused(Direction);
    /// Activate the n-th (0-based) tab of the active pane.
    ActivateTab(usize);
    /// Move the focused panel (dock tab or panel tab) to a dock or into the center.
    MoveActivePanel(PanelDest);
    /// Move the active tab into a new split at this edge of its pane.
    MoveActiveTabToSplit(Edge);
}

/// A palette entry: human title, the action it runs, and its default key (if any).
pub struct Command {
    pub title: SharedString,
    pub action: Box<dyn Action>,
    pub binding: Option<KeyBinding>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("title", &self.title)
            .field("action", &self.action.name())
            .finish_non_exhaustive()
    }
}

fn command<A: Action + Clone>(
    title: impl Into<SharedString>,
    action: A,
    keys: Option<&str>,
) -> Command {
    Command {
        title: title.into(),
        binding: keys.map(|k| KeyBinding::new(k, action.clone(), None)),
        action: Box::new(action),
    }
}

fn dir_name(dir: Direction) -> &'static str {
    match dir {
        Direction::Left => "Left",
        Direction::Right => "Right",
        Direction::Up => "Up",
        Direction::Down => "Down",
    }
}

fn dir_key(dir: Direction) -> &'static str {
    match dir {
        Direction::Left => "left",
        Direction::Right => "right",
        Direction::Up => "up",
        Direction::Down => "down",
    }
}

fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "Left",
        Edge::Right => "Right",
        Edge::Top => "Up",
        Edge::Bottom => "Down",
    }
}

/// Every palette command. Order is the order shown for an empty query.
pub fn commands(presets: &[LayoutPreset], panels: &PanelRegistry) -> Vec<Command> {
    let mut out = vec![
        command("Layout: Split Right", SplitRight, Some("cmd-d")),
        command("Layout: Split Down", SplitDown, Some("cmd-shift-d")),
        command(
            "Layout: Toggle Zoom Pane",
            ToggleZoom,
            Some("cmd-shift-enter"),
        ),
        command("Layout: Reset Layout", ResetLayout, None),
        command("Tab: New Tab", NewTab, Some("cmd-t")),
        command("Tab: Close", ClosePaneItem, Some("cmd-w")),
        command("Tab: Open Panel as Tab", OpenActivePanelAsTab, None),
        command("View: Toggle Left Dock", ToggleLeftDock, Some("cmd-b")),
        command(
            "View: Toggle Right Dock",
            ToggleRightDock,
            Some("cmd-alt-b"),
        ),
        command("View: Toggle Bottom Dock", ToggleBottomDock, Some("cmd-j")),
        command("View: Toggle Overview", ToggleOverview, Some("cmd-shift-o")),
        command("View: Density Compact", SetDensityCompact, None),
        command("View: Density Comfortable", SetDensityComfortable, None),
        command("View: Toggle Density", ToggleDensity, None),
        command("App: Command Palette", TogglePalette, Some("cmd-k")),
        command("App: Settings…", OpenSettings, Some("cmd-,")),
        command("App: About Heeler", About, None),
        command("Window: Minimize", MinimizeWindow, Some("cmd-m")),
        command("Window: Zoom", ZoomWindow, None),
        command("App: Quit Heeler", Quit, Some("cmd-q")),
    ];
    for dir in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        out.push(command(
            format!("Focus: Neighbor {}", dir_name(dir)),
            FocusNeighbor(dir),
            Some(&format!("cmd-alt-{}", dir_key(dir))),
        ));
    }
    for dir in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        out.push(command(
            format!("Layout: Resize {}", dir_name(dir)),
            ResizeFocused(dir),
            Some(&format!("ctrl-cmd-{}", dir_key(dir))),
        ));
    }
    for edge in [Edge::Right, Edge::Bottom, Edge::Left, Edge::Top] {
        out.push(command(
            format!("Layout: Move Tab to New Split {}", edge_name(edge)),
            MoveActiveTabToSplit(edge),
            None,
        ));
    }
    for pos in DockPosition::ALL {
        out.push(command(
            format!("Panel: Move to {} Dock", pos.label()),
            MoveActivePanel(PanelDest::Dock(pos)),
            None,
        ));
    }
    for (i, preset) in presets.iter().enumerate() {
        // ⌘⇧1…9 switch presets by position; overview additionally has its own toggle.
        let keys = (i < 9).then(|| format!("cmd-shift-{}", i + 1));
        out.push(command(
            format!("Layout: Apply Preset {}", preset.name),
            ApplyPreset(preset.name.clone()),
            keys.as_deref(),
        ));
    }
    for spec in panels.iter() {
        out.push(command(
            format!("Panel: Open {}", spec.title),
            OpenPanel(spec.id.to_string()),
            None,
        ));
    }
    out
}

/// Default key bindings: every command's key, ⌘1…⌘9 tab selection, and the palette-local keys.
pub fn default_keymap(commands: &[Command]) -> Vec<KeyBinding> {
    let mut bindings: Vec<KeyBinding> = commands.iter().filter_map(|c| c.binding.clone()).collect();
    for n in 1..=9usize {
        bindings.push(KeyBinding::new(
            &format!("cmd-{n}"),
            ActivateTab(n - 1),
            None,
        ));
    }
    bindings.extend([
        KeyBinding::new("down", palette::SelectNext, Some("Palette")),
        KeyBinding::new("ctrl-n", palette::SelectNext, Some("Palette")),
        KeyBinding::new("up", palette::SelectPrev, Some("Palette")),
        KeyBinding::new("ctrl-p", palette::SelectPrev, Some("Palette")),
        KeyBinding::new("enter", palette::Confirm, Some("Palette")),
        KeyBinding::new("escape", palette::Dismiss, Some("Palette")),
        KeyBinding::new("escape", palette::Dismiss, Some("ContextMenu")),
    ]);
    bindings
}
