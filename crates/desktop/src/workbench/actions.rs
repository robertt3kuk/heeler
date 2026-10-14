//! Named workbench actions, the command table behind the ⌘K palette, and the default keymap.
//!
//! One list feeds three consumers (ADR 0010): the palette, the menus, and the keymap. Every
//! action that has a key by default is parameterless so `keymap.toml` can name it
//! (`workbench::SplitDown`); actions with data (`ApplyPreset(name)`, `OpenSession(id)`, …) are
//! reachable from the palette, menus and panels.

use super::model::{Direction, DockPosition, Edge, LayoutPreset, PanelDest};
use super::panels::{HeelerTarget, HistoryResume};
use super::registry::PanelRegistry;
use crate::settings::{OnQuit, WorkspaceSort};
use gpui::{Action, App, KeyBinding, KeyBindingContextPredicate, SharedString, actions};
use heeler_settings::keymap::{BindingSpec, KeymapLoad, format_keys};

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
        NewTerminal,
        NewAgent,
        NewChat,
        StopSession,
        RenameSession,
        RestartSession,
        CopySessionId,
        JumpToNextAttention,
        SaveLayoutAsPreset,
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
        FocusNeighborLeft,
        FocusNeighborRight,
        FocusNeighborUp,
        FocusNeighborDown,
        ResizeLeft,
        ResizeRight,
        ResizeUp,
        ResizeDown,
        ActivateTab1,
        ActivateTab2,
        ActivateTab3,
        ActivateTab4,
        ActivateTab5,
        ActivateTab6,
        ActivateTab7,
        ActivateTab8,
        ActivateTab9,
        ApplyPresetNumber1,
        ApplyPresetNumber2,
        ApplyPresetNumber3,
        ApplyPresetNumber4,
        ApplyPresetNumber5,
        ApplyPresetNumber6,
        ApplyPresetNumber7,
        ApplyPresetNumber8,
        ApplyPresetNumber9,
        OpenProject,
        OpenFolder,
        OpenJump,
        GoHome,
        PinActiveWorkspace,
        RenameActiveWorkspace,
        AddFolderToWorkspace,
        CloseActiveWorkspace,
        RevealWorkspace,
        ToggleHideEmptyWorkspaces,
        ToggleHideSleepingWorkspaces,
        ToggleHideCliWorkspaces,
        SearchHistory,
        ReviewApprovals,
    ]
);

/// Palette-local actions, bound only inside the `Palette` key context.
pub mod palette {
    use gpui::actions;
    actions!(palette, [SelectNext, SelectPrev, Confirm, Dismiss]);
}

/// Actions of the prompt/confirm dialog (`Modal` key context).
pub mod modal {
    use gpui::actions;
    actions!(modal, [Confirm, Cancel]);
}

/// Actions of the agent chat view (`HeelerChat` key context).
pub mod chat {
    use gpui::actions;
    actions!(chat, [SendMessage, InterruptTurn]);
}

/// Actions of the settings sheet (`SettingsSheet` key context).
pub mod sheet {
    use gpui::actions;
    actions!(settings_sheet, [Dismiss]);
}

/// Daemon controls (design.md § Daemon lifecycle). The `Daemon` menu, the status-bar popover
/// and the palette all dispatch these, so there is one implementation.
pub mod daemon {
    use gpui::actions;
    actions!(daemon, [Status, Restart, Stop, Start, OpenLogs]);
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

/// A session to open next to the focused pane.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SessionSplit {
    pub session: String,
    pub edge: Edge,
}

/// Where to open the context menu of a session.
#[derive(Clone, PartialEq, Debug)]
pub struct SessionMenuAt {
    pub session: String,
    pub position: gpui::Point<gpui::Pixels>,
}

data_actions! {
    /// Apply the named layout preset (`default`, `focus`, `overview`, …).
    ApplyPreset(String);
    /// Reveal the panel with this id, opening it in its default dock if it is closed.
    OpenPanel(String);
    /// Move the focused panel (dock tab or panel tab) to a dock or into the center.
    MoveActivePanel(PanelDest);
    /// Move the active tab into a new split at this edge of its pane.
    MoveActiveTabToSplit(Edge);
    /// Open (or focus the tab of) the session with this id.
    OpenSession(String);
    /// Open the session in a new split of the focused pane.
    OpenSessionInSplit(SessionSplit);
    StopSessionById(String);
    RenameSessionById(String);
    RestartSessionById(String);
    CopySessionIdOf(String);
    /// Show the session's context menu (Open in split, Rename, Restart, Stop, Copy id).
    ShowSessionMenu(SessionMenuAt);
    /// Leave the overview (restoring the layout it replaced) and focus this session.
    OpenSessionFromOverview(String);
    /// Open the settings sheet at a namespace (`terminal`, `panels.sessions`, …).
    OpenSettingsFor(String);
    /// Set the working mode (`app.on_quit`): what quitting does with the daemon.
    SetOnQuit(OnQuit);
    /// Scroll the terminal of the session to a block and highlight it.
    JumpToBlock(BlockRef);
    /// Show the workspace with this id (switching the center to its layout).
    OpenWorkspace(String);
    /// Show the context menu of a workspace row.
    ShowWorkspaceMenu(WorkspaceMenuAt);
    /// Show the menu of workspace sort modes at this position.
    ShowSortMenu(gpui::Point<gpui::Pixels>);
    /// Sort the sidebar's workspaces (`workspaces.sort`).
    SetWorkspaceSort(WorkspaceSort);
    /// A workspace row was dropped onto another one (manual reordering).
    MoveWorkspaceOnto(WorkspaceDrop);
    /// Resume a session found in the history index: its command in a new terminal session.
    ResumeHistoryHit(HistoryResume);
    /// Copy a resume command line to the clipboard.
    CopyHistoryCommand(String);
    /// Show the Heeler session (and block) a history hit points at.
    OpenHistoryHit(HeelerTarget);
}

/// Where to open the context menu of a workspace.
#[derive(Clone, PartialEq, Debug)]
pub struct WorkspaceMenuAt {
    pub workspace: String,
    pub position: gpui::Point<gpui::Pixels>,
}

/// A dragged workspace row and the row it was dropped on (workspace ids).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct WorkspaceDrop {
    pub dragged: String,
    pub target: String,
}

/// A block of a session's terminal (`block` is the daemon's block id).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BlockRef {
    pub session: String,
    pub block: u64,
}

/// A palette entry: human title, the action it runs, and the action whose key binding is shown
/// as its hint.
pub struct Command {
    pub title: SharedString,
    pub action: Box<dyn Action>,
    pub hint: Box<dyn Action>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Command")
            .field("title", &self.title)
            .field("action", &self.action.name())
            .finish_non_exhaustive()
    }
}

fn command<A: Action + Clone>(title: impl Into<SharedString>, action: A) -> Command {
    Command {
        title: title.into(),
        hint: Box::new(action.clone()),
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

fn edge_name(edge: Edge) -> &'static str {
    match edge {
        Edge::Left => "Left",
        Edge::Right => "Right",
        Edge::Top => "Up",
        Edge::Bottom => "Down",
    }
}

/// The parameterless action for focusing the neighbor in `dir`.
pub fn focus_neighbor_action(dir: Direction) -> Box<dyn Action> {
    match dir {
        Direction::Left => Box::new(FocusNeighborLeft),
        Direction::Right => Box::new(FocusNeighborRight),
        Direction::Up => Box::new(FocusNeighborUp),
        Direction::Down => Box::new(FocusNeighborDown),
    }
}

/// The parameterless action for resizing towards `dir`.
pub fn resize_action(dir: Direction) -> Box<dyn Action> {
    match dir {
        Direction::Left => Box::new(ResizeLeft),
        Direction::Right => Box::new(ResizeRight),
        Direction::Up => Box::new(ResizeUp),
        Direction::Down => Box::new(ResizeDown),
    }
}

/// The parameterless action selecting the `n`-th (1-based) preset.
pub fn preset_number_action(n: usize) -> Option<Box<dyn Action>> {
    Some(match n {
        1 => Box::new(ApplyPresetNumber1),
        2 => Box::new(ApplyPresetNumber2),
        3 => Box::new(ApplyPresetNumber3),
        4 => Box::new(ApplyPresetNumber4),
        5 => Box::new(ApplyPresetNumber5),
        6 => Box::new(ApplyPresetNumber6),
        7 => Box::new(ApplyPresetNumber7),
        8 => Box::new(ApplyPresetNumber8),
        9 => Box::new(ApplyPresetNumber9),
        _ => return None,
    })
}

/// Every palette command. Order is the order shown for an empty query.
pub fn commands(presets: &[LayoutPreset], panels: &PanelRegistry) -> Vec<Command> {
    let mut out = vec![
        command("Session: New Terminal", NewTerminal),
        command("Session: New Agent…", NewAgent),
        command("Session: New Chat (arrives with M5)", NewChat),
        command("Session: Rename…", RenameSession),
        command("Session: Restart", RestartSession),
        command("Session: Stop…", StopSession),
        command("Session: Copy Session ID", CopySessionId),
        command("Session: Jump to Next That Needs You", JumpToNextAttention),
        command("Layout: Split Right", SplitRight),
        command("Layout: Split Down", SplitDown),
        command("Layout: Toggle Zoom Pane", ToggleZoom),
        command("Layout: Reset Layout", ResetLayout),
        command("Layout: Save Layout as Preset…", SaveLayoutAsPreset),
        command("Tab: Close (Detach)", ClosePaneItem),
        command("Tab: Open Panel as Tab", OpenActivePanelAsTab),
        command("View: Toggle Left Dock", ToggleLeftDock),
        command("View: Toggle Right Dock", ToggleRightDock),
        command("View: Toggle Bottom Dock", ToggleBottomDock),
        command("View: Toggle Overview", ToggleOverview),
        command("View: Density Compact", SetDensityCompact),
        command("View: Density Comfortable", SetDensityComfortable),
        command("View: Toggle Density", ToggleDensity),
        command("App: Command Palette", TogglePalette),
        command("App: Settings…", OpenSettings),
        command("App: About Heeler", About),
        command("Window: Minimize", MinimizeWindow),
        command("Window: Zoom", ZoomWindow),
        command("App: Quit Heeler", Quit),
        command("Daemon: Status", daemon::Status),
        command("Daemon: Restart Daemon", daemon::Restart),
        command("Daemon: Stop Daemon", daemon::Stop),
        command("Daemon: Start Daemon", daemon::Start),
        command("Daemon: Open Logs", daemon::OpenLogs),
        command("Daemon: Working Mode — Ask on Quit", SetOnQuit(OnQuit::Ask)),
        command(
            "Daemon: Working Mode — Keep Sessions Running After Quit",
            SetOnQuit(OnQuit::KeepRunning),
        ),
        command(
            "Daemon: Working Mode — Stop Daemon With the App",
            SetOnQuit(OnQuit::StopDaemon),
        ),
    ];
    out.extend([
        command("Workspace: Open Project…", OpenProject),
        command("Workspace: Open Folder…", OpenFolder),
        command("Workspace: Jump to Workspace or Session…", OpenJump),
        command("Workspace: Go to Home", GoHome),
        command(
            "Workspace: Pin / Unpin Active Workspace",
            PinActiveWorkspace,
        ),
        command("Workspace: Rename Active Workspace…", RenameActiveWorkspace),
        command(
            "Workspace: Add Folder to Active Workspace…",
            AddFolderToWorkspace,
        ),
        command("Workspace: Close Active Workspace", CloseActiveWorkspace),
        command(
            "Workspace: Reveal Active Workspace in Finder",
            RevealWorkspace,
        ),
    ]);
    out.extend(WorkspaceSort::ALL.map(|mode| {
        command(
            format!("Workspace: Sort — {}", mode.label()),
            SetWorkspaceSort(mode),
        )
    }));
    out.extend([
        command(
            "Workspace: Toggle Hide Empty Workspaces",
            ToggleHideEmptyWorkspaces,
        ),
        command(
            "Workspace: Toggle Hide Sleeping Workspaces",
            ToggleHideSleepingWorkspaces,
        ),
        command(
            "Workspace: Toggle Hide CLI-Created Workspaces",
            ToggleHideCliWorkspaces,
        ),
    ]);
    out.push(command("History: Search History…", SearchHistory));
    out.extend([
        command("Agent: Review Approvals…", ReviewApprovals),
        command("Agent: Send Message", chat::SendMessage),
        command("Agent: Interrupt Turn", chat::InterruptTurn),
    ]);
    out.extend([
        command(
            "Block: Previous Prompt",
            heeler_terminal_view::JumpToPreviousPrompt,
        ),
        command("Block: Next Prompt", heeler_terminal_view::JumpToNextPrompt),
        command(
            "Block: Select Output",
            heeler_terminal_view::SelectBlockOutput,
        ),
        command("Block: Copy Output", heeler_terminal_view::CopyBlockOutput),
        command(
            "Block: Copy Command",
            heeler_terminal_view::CopyBlockCommand,
        ),
        command(
            "Block: Collapse / Expand",
            heeler_terminal_view::ToggleBlockCollapse,
        ),
        command("Block: Rerun", heeler_terminal_view::RerunBlock),
        command(
            "Block: Send to Agent",
            heeler_terminal_view::SendBlockToAgent,
        ),
    ]);
    for dir in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        out.push(Command {
            title: format!("Focus: Neighbor {}", dir_name(dir)).into(),
            action: focus_neighbor_action(dir),
            hint: focus_neighbor_action(dir),
        });
    }
    for dir in [
        Direction::Left,
        Direction::Right,
        Direction::Up,
        Direction::Down,
    ] {
        out.push(Command {
            title: format!("Layout: Resize {}", dir_name(dir)).into(),
            action: resize_action(dir),
            hint: resize_action(dir),
        });
    }
    for edge in [Edge::Right, Edge::Bottom, Edge::Left, Edge::Top] {
        out.push(command(
            format!("Layout: Move Tab to New Split {}", edge_name(edge)),
            MoveActiveTabToSplit(edge),
        ));
    }
    for pos in DockPosition::ALL {
        out.push(command(
            format!("Panel: Move to {} Dock", pos.label()),
            MoveActivePanel(PanelDest::Dock(pos)),
        ));
    }
    for (i, preset) in presets.iter().enumerate() {
        let apply = ApplyPreset(preset.name.clone());
        let title = format!("Layout: Apply Preset {}", preset.name);
        match preset_number_action(i + 1) {
            // ⌘⇧1…9 switch presets by position.
            Some(number) => out.push(Command {
                title: title.into(),
                action: Box::new(apply),
                hint: number,
            }),
            None => out.push(command(title, apply)),
        }
    }
    for spec in panels.iter() {
        out.push(command(
            format!("Panel: Open {}", spec.title),
            OpenPanel(spec.id.to_string()),
        ));
    }
    out
}

fn bind<A: Action>(keys: &str, context: Option<&str>) -> BindingSpec {
    match BindingSpec::new(keys, A::name_for_type(), context) {
        Ok(spec) => spec,
        // The defaults are literals; a typo is a bug caught by `default_bindings_parse`.
        Err(err) => {
            tracing::error!("invalid default key `{keys}`: {err}");
            BindingSpec {
                keys: Vec::new(),
                action: A::name_for_type().to_owned(),
                context: context.map(str::to_owned),
            }
        }
    }
}

/// The key bindings Heeler ships with; `keymap.toml` is merged on top.
pub fn default_bindings() -> Vec<BindingSpec> {
    let mut out = vec![
        bind::<TogglePalette>("cmd-k", None),
        bind::<OpenSettings>("cmd-,", None),
        bind::<NewTerminal>("cmd-t", None),
        bind::<NewAgent>("cmd-shift-t", None),
        bind::<NewChat>("cmd-alt-t", None),
        bind::<JumpToNextAttention>("cmd-shift-j", None),
        bind::<SplitRight>("cmd-d", None),
        bind::<SplitDown>("cmd-shift-d", None),
        bind::<ClosePaneItem>("cmd-w", None),
        bind::<StopSession>("cmd-shift-w", None),
        bind::<ToggleZoom>("cmd-shift-enter", None),
        bind::<ToggleLeftDock>("cmd-b", None),
        bind::<ToggleRightDock>("cmd-alt-b", None),
        bind::<ToggleBottomDock>("cmd-alt-j", None),
        bind::<OpenJump>("cmd-j", None),
        bind::<OpenProject>("cmd-o", None),
        bind::<ToggleOverview>("cmd-shift-o", None),
        bind::<MinimizeWindow>("cmd-m", None),
        bind::<Quit>("cmd-q", None),
        bind::<FocusNeighborLeft>("cmd-alt-left", None),
        bind::<FocusNeighborRight>("cmd-alt-right", None),
        bind::<FocusNeighborUp>("cmd-alt-up", None),
        bind::<FocusNeighborDown>("cmd-alt-down", None),
        bind::<ResizeLeft>("ctrl-cmd-left", None),
        bind::<ResizeRight>("ctrl-cmd-right", None),
        bind::<ResizeUp>("ctrl-cmd-up", None),
        bind::<ResizeDown>("ctrl-cmd-down", None),
        bind::<heeler_terminal_view::Copy>("cmd-c", Some(heeler_terminal_view::KEY_CONTEXT)),
        bind::<heeler_terminal_view::Paste>("cmd-v", Some(heeler_terminal_view::KEY_CONTEXT)),
        bind::<heeler_terminal_view::JumpToPreviousPrompt>(
            "cmd-up",
            Some(heeler_terminal_view::KEY_CONTEXT),
        ),
        bind::<heeler_terminal_view::JumpToNextPrompt>(
            "cmd-down",
            Some(heeler_terminal_view::KEY_CONTEXT),
        ),
        bind::<heeler_terminal_view::SelectBlockOutput>(
            "cmd-shift-a",
            Some(heeler_terminal_view::KEY_CONTEXT),
        ),
        bind::<heeler_terminal_view::CopyBlockOutput>(
            "cmd-shift-c",
            Some(heeler_terminal_view::KEY_CONTEXT),
        ),
        bind::<palette::SelectNext>("down", Some("Palette")),
        bind::<palette::SelectNext>("ctrl-n", Some("Palette")),
        bind::<palette::SelectPrev>("up", Some("Palette")),
        bind::<palette::SelectPrev>("ctrl-p", Some("Palette")),
        bind::<palette::Confirm>("enter", Some("Palette")),
        bind::<palette::Dismiss>("escape", Some("Palette")),
        bind::<palette::Dismiss>("escape", Some("ContextMenu")),
        bind::<modal::Confirm>("enter", Some("Modal")),
        bind::<modal::Cancel>("escape", Some("Modal")),
        bind::<sheet::Dismiss>("escape", Some("SettingsSheet")),
        bind::<palette::Dismiss>("escape", Some("DaemonPopover")),
        bind::<chat::SendMessage>("cmd-enter", Some(super::chat::KEY_CONTEXT)),
        bind::<chat::InterruptTurn>("cmd-.", Some(super::chat::KEY_CONTEXT)),
        bind::<palette::SelectNext>("down", Some("AgentPicker")),
        bind::<palette::SelectNext>("ctrl-n", Some("AgentPicker")),
        bind::<palette::SelectPrev>("up", Some("AgentPicker")),
        bind::<palette::SelectPrev>("ctrl-p", Some("AgentPicker")),
        bind::<palette::Confirm>("enter", Some("AgentPicker")),
        bind::<palette::Dismiss>("escape", Some("AgentPicker")),
        bind::<palette::SelectNext>("down", Some("Picker")),
        bind::<palette::SelectNext>("ctrl-n", Some("Picker")),
        bind::<palette::SelectPrev>("up", Some("Picker")),
        bind::<palette::SelectPrev>("ctrl-p", Some("Picker")),
        bind::<palette::Confirm>("enter", Some("Picker")),
        bind::<palette::Dismiss>("escape", Some("Picker")),
    ];
    let tabs: [&str; 9] = [
        ActivateTab1::name_for_type(),
        ActivateTab2::name_for_type(),
        ActivateTab3::name_for_type(),
        ActivateTab4::name_for_type(),
        ActivateTab5::name_for_type(),
        ActivateTab6::name_for_type(),
        ActivateTab7::name_for_type(),
        ActivateTab8::name_for_type(),
        ActivateTab9::name_for_type(),
    ];
    let presets: [&str; 9] = [
        ApplyPresetNumber1::name_for_type(),
        ApplyPresetNumber2::name_for_type(),
        ApplyPresetNumber3::name_for_type(),
        ApplyPresetNumber4::name_for_type(),
        ApplyPresetNumber5::name_for_type(),
        ApplyPresetNumber6::name_for_type(),
        ApplyPresetNumber7::name_for_type(),
        ApplyPresetNumber8::name_for_type(),
        ApplyPresetNumber9::name_for_type(),
    ];
    for n in 1..=9usize {
        for (keys, action) in [
            (format!("cmd-{n}"), tabs[n - 1]),
            (format!("cmd-shift-{n}"), presets[n - 1]),
        ] {
            match BindingSpec::new(&keys, action, None) {
                Ok(spec) => out.push(spec),
                Err(err) => tracing::error!("invalid default key `{keys}`: {err}"),
            }
        }
    }
    out
}

/// Replace every key binding of the app with `load.bindings`. Bindings naming an unknown action
/// or an unparsable context are skipped with a warning.
pub fn apply_keymap(cx: &mut App, load: &KeymapLoad) {
    let mut bindings = Vec::with_capacity(load.bindings.len());
    for spec in &load.bindings {
        if spec.keys.is_empty() {
            continue;
        }
        let action = match cx.build_action(&spec.action, None) {
            Ok(action) => action,
            Err(err) => {
                tracing::warn!("keymap: action `{}` cannot be bound: {err}", spec.action);
                continue;
            }
        };
        let predicate = match spec
            .context
            .as_deref()
            .map(KeyBindingContextPredicate::parse)
        {
            None => None,
            Some(Ok(predicate)) => Some(predicate.into()),
            Some(Err(err)) => {
                tracing::warn!(
                    "keymap: context `{}` of `{}` is invalid: {err}",
                    spec.context.as_deref().unwrap_or_default(),
                    spec.action
                );
                continue;
            }
        };
        match KeyBinding::load(
            &format_keys(&spec.keys),
            action,
            predicate,
            false,
            None,
            cx.keyboard_mapper().as_ref(),
        ) {
            Ok(binding) => bindings.push(binding),
            Err(err) => tracing::warn!("keymap: `{}`: {err}", format_keys(&spec.keys)),
        }
    }
    cx.clear_key_bindings();
    cx.bind_keys(bindings);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_bindings_parse_and_do_not_collide() {
        let defaults = default_bindings();
        assert!(defaults.iter().all(|b| !b.keys.is_empty()));
        let load = heeler_settings::keymap::load_str(&defaults, "", None).unwrap();
        assert!(load.diagnostics.is_empty(), "{:?}", load.diagnostics);
        assert_eq!(load.bindings.len(), defaults.len());
    }
}
