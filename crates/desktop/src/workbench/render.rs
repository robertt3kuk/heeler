//! Rendering and action wiring for [`Workbench`].

use super::actions::{self, palette};
use super::model::{
    Axis, Direction, DockPosition, DropTarget, Edge, FocusTarget, ItemRef, ItemSource,
    LayoutPreset, Pane, PaneId, PaneTree, PanelDest,
};
use super::registry::{StatusContext, StatusData};
use super::terminals::session_of_item;
use super::view::{
    ContextMenu, DragPreview, DraggedItem, Region, Resizing, Workbench, preset_label, steps,
};
use super::workspaces::PickerKind;
use crate::theme::{Density, DensityExt as _};
use gpui::{
    Action, AnyElement, Bounds, ClickEvent, Context, ElementId, InteractiveElement, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, ParentElement, Pixels, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, anchored, canvas, deferred, div, point, prelude::*,
    px, relative,
};
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;

/// Height of the title bar region (and of the inset above the left dock's panel stack).
pub const TITLEBAR_HEIGHT: f32 = 36.0;
/// Space reserved for the traffic lights when the left dock does not cover them.
const TRAFFIC_LIGHT_INSET: f32 = 80.0;
const HANDLE_THICKNESS: f32 = 6.0;

/// "⌘⇧O"-style hint for the highest-precedence binding of `action` in the current context.
fn key_hint(window: &Window, action: &dyn Action) -> Option<SharedString> {
    let binding = window.highest_precedence_binding_for_action(action)?;
    let text: Vec<String> = binding.keystrokes().iter().map(|k| k.to_string()).collect();
    Some(text.join(" ").into())
}

/// An invisible child that records its parent's painted bounds under `region`.
pub(super) fn probe(
    regions: &Rc<RefCell<HashMap<Region, Bounds<Pixels>>>>,
    region: Region,
) -> impl IntoElement {
    let regions = Rc::clone(regions);
    canvas(
        move |bounds, _, _| {
            regions.borrow_mut().insert(region, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

fn id(text: impl Into<SharedString>) -> ElementId {
    ElementId::from(text.into())
}

impl Render for Workbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_appearance(window, cx);
        let title = format!("{} — Heeler", self.active_title(cx));
        if title != self.last_title {
            window.set_window_title(&title);
            self.last_title = title;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        self.regions.borrow_mut().clear();
        self.rendered_terminals.clear();
        self.rendered_chats.clear();

        let left = self.render_dock(DockPosition::Left, window, cx);
        let right = self.render_dock(DockPosition::Right, window, cx);
        let bottom = self.render_dock(DockPosition::Bottom, window, cx);
        let center = self.render_center(window, cx);
        let titlebar = self.render_titlebar(window, cx);
        let status = self.render_status_bar(window, cx);
        self.rescue_orphaned_focus(window, cx);
        let edge_targets = self.render_hidden_dock_targets(cx);
        let palette = self.render_palette(window, cx);
        let picker = self.render_picker(window, cx);
        let agent_picker = self.render_agent_picker(window, cx);
        let menu = self.render_context_menu(cx);
        let modal = self.render_modal(cx);
        let daemon_popover = self.render_daemon_popover(cx);
        let sheet = self.render_settings_sheet(cx);

        let root = div()
            .id("workbench")
            .key_context("Workbench")
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .on_mouse_move(cx.listener(|this, event, window, cx| {
                this.on_mouse_move(event, window, cx);
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, _| this.on_mouse_up()),
            );
        let root = self.bind_actions(root, cx);

        root.child(
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .w_full()
                .children(left)
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(titlebar)
                        .child(
                            div()
                                .flex()
                                .flex_1()
                                .min_h_0()
                                .child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .flex_1()
                                        .min_w_0()
                                        .child(center)
                                        .children(bottom),
                                )
                                .children(right),
                        ),
                ),
        )
        .child(status)
        .children(daemon_popover)
        .children(edge_targets)
        .children(palette)
        .children(picker)
        .children(agent_picker)
        .children(menu)
        .children(sheet)
        .children(modal)
    }
}

impl Workbench {
    // ---- actions -----------------------------------------------------------------------------

    fn bind_actions<E: InteractiveElement>(&self, root: E, cx: &mut Context<Self>) -> E {
        let (dock_step, split_step) = steps();
        root.on_action(cx.listener(|this, _: &actions::SplitRight, window, cx| {
            this.split_active(Edge::Right, window, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::SplitDown, window, cx| {
            this.split_active(Edge::Bottom, window, cx)
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleLeftDock, _, cx| {
            this.mutate(cx, |s| s.toggle_dock(DockPosition::Left));
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleRightDock, _, cx| {
            this.mutate(cx, |s| s.toggle_dock(DockPosition::Right));
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleBottomDock, _, cx| {
            this.mutate(cx, |s| s.toggle_dock(DockPosition::Bottom));
        }))
        .on_action(cx.listener(|this, _: &actions::ToggleOverview, _, cx| this.toggle_overview(cx)))
        .on_action(cx.listener(|this, _: &actions::ClosePaneItem, _, cx| {
            this.mutate(cx, |s| {
                s.close_active_item();
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleZoom, _, cx| {
                this.mutate(cx, |s| s.toggle_zoom())
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ResetLayout, _, cx| this.reset_layout(cx)))
        .on_action(cx.listener(|this, _: &actions::TogglePalette, window, cx| {
            this.toggle_palette(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::OpenSettings, window, cx| {
            this.open_settings(None, window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenSettingsFor, window, cx| {
                this.open_settings(Some(&a.0), window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::About, _, cx| {
            this.set_notice(concat!("Heeler desktop ", env!("CARGO_PKG_VERSION")), cx);
        }))
        .on_action(
            cx.listener(|_, _: &actions::MinimizeWindow, window, _| window.minimize_window()),
        )
        .on_action(cx.listener(|_, _: &actions::ZoomWindow, window, _| window.zoom_window()))
        .on_action(cx.listener(|this, _: &actions::ToggleDensity, _, cx| {
            let next = this.density().toggled();
            this.set_density(next, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::SetDensityCompact, _, cx| {
            this.set_density(Density::Compact, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::SetDensityComfortable, _, cx| {
                this.set_density(Density::Comfortable, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::OpenActivePanelAsTab, _, cx| {
                this.move_focused_panel(PanelDest::Center, cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ApplyPreset, _, cx| this.apply_preset_named(&a.0, cx)),
        )
        .on_action(cx.listener(|this, a: &actions::OpenPanel, _, cx| this.open_panel(&a.0, cx)))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborLeft, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Left);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborRight, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Right);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborUp, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Up);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::FocusNeighborDown, _, cx| {
            this.mutate(cx, |s| {
                s.focus_neighbor(Direction::Down);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeLeft, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Left, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeRight, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Right, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeUp, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Up, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(move |this, _: &actions::ResizeDown, _, cx| {
            this.mutate(cx, |s| {
                s.resize_focused(Direction::Down, dock_step, split_step);
            });
        }))
        .on_action(cx.listener(|this, _: &actions::ActivateTab1, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(0);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber1, window, cx| {
                this.apply_preset_number(1, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab2, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(1);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber2, window, cx| {
                this.apply_preset_number(2, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab3, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(2);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber3, window, cx| {
                this.apply_preset_number(3, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab4, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(3);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber4, window, cx| {
                this.apply_preset_number(4, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab5, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(4);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber5, window, cx| {
                this.apply_preset_number(5, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab6, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(5);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber6, window, cx| {
                this.apply_preset_number(6, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab7, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(6);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber7, window, cx| {
                this.apply_preset_number(7, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab8, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(7);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber8, window, cx| {
                this.apply_preset_number(8, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ActivateTab9, _, cx| {
            this.mutate(cx, |s| {
                s.select_tab(8);
            });
        }))
        .on_action(
            cx.listener(|this, _: &actions::ApplyPresetNumber9, window, cx| {
                this.apply_preset_number(9, window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NewTerminal, window, cx| {
            this.new_terminal(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::NewAgent, window, cx| {
            this.new_agent(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ReviewApprovals, window, cx| {
                this.open_approvals_picker(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::NewChat, _, cx| {
            this.set_notice("Chats arrive with M5", cx);
        }))
        .on_action(
            cx.listener(|this, _: &heeler_terminal_view::SendBlockToAgent, _, cx| {
                this.set_notice(
                    "Focus a terminal with a finished command to send it to an agent",
                    cx,
                );
            }),
        )
        .on_action(cx.listener(|this, _: &actions::SearchHistory, window, cx| {
            this.search_history(window, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::ResumeHistoryHit, window, cx| {
                this.resume_history_hit(a.0.clone(), window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::CopyHistoryCommand, _, cx| {
            this.copy_history_command(&a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenHistoryHit, window, cx| {
                this.open_history_hit(a.0.clone(), window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::ShowAgentMonitor, _, cx| {
            this.show_agent_monitor(cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::ShowOnlyBlockedAgents, _, cx| {
                this.show_only_blocked_agents(cx);
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::ShowAgentNodeMenu, window, cx| {
                if let Ok(session) = a.0.session.parse() {
                    this.show_agent_node_menu(session, &a.0.node, a.0.position, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::JumpToBlock, window, cx| {
            this.jump_to_block(&a.0.session, a.0.block, window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::StopSession, window, cx| {
            if let Some(session) = this.focused_session() {
                this.stop_session(session, window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::RenameSession, window, cx| {
            if let Some(session) = this.focused_session() {
                this.rename_session(session, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::RestartSession, window, cx| {
                if let Some(session) = this.focused_session() {
                    this.restart_session(session, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, _: &actions::CopySessionId, _, cx| {
            if let Some(session) = this.focused_session() {
                this.copy_session_id(session, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::JumpToNextAttention, window, cx| {
                this.jump_to_next_attention(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::SaveLayoutAsPreset, window, cx| {
                this.save_layout_as_preset(window, cx);
            }),
        )
        .on_action(cx.listener(|this, a: &actions::OpenSession, window, cx| {
            if let Ok(id) = a.0.parse() {
                this.open_session(id, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::OpenSessionInSplit, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    this.open_session_in_split(id, a.0.edge, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::OpenSessionFromOverview, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.open_session_from_overview(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::StopSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.stop_session(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::RenameSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.rename_session(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, a: &actions::RestartSessionById, window, cx| {
                if let Ok(id) = a.0.parse() {
                    this.restart_session(id, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::CopySessionIdOf, _, cx| {
            if let Ok(id) = a.0.parse() {
                this.copy_session_id(id, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowSessionMenu, window, cx| {
                if let Ok(id) = a.0.session.parse() {
                    let entries = this.session_menu(id);
                    this.open_menu(a.0.position, entries, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::MoveActivePanel, _, cx| {
            this.move_focused_panel(a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::MoveActiveTabToSplit, _, cx| {
                this.move_active_tab_to_split(a.0, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &palette::Dismiss, window, cx| {
            this.close_menu(window, cx);
            this.close_palette(window, cx);
            this.close_picker(window, cx);
            this.close_agent_picker(window, cx);
            this.close_daemon_popover(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::Quit, window, cx| {
            this.request_quit(window, cx);
        }))
        .on_action(
            cx.listener(|this, _: &actions::daemon::Status, window, cx| {
                this.show_daemon_status(window, cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::daemon::Restart, window, cx| {
                this.restart_daemon(window, cx);
            }),
        )
        .on_action(cx.listener(|this, _: &actions::daemon::Stop, window, cx| {
            this.stop_daemon(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::daemon::Start, _, cx| {
            this.start_daemon(cx);
        }))
        .on_action(cx.listener(|this, _: &actions::daemon::OpenLogs, _, cx| {
            this.open_daemon_logs(cx);
        }))
        .on_action(cx.listener(|this, a: &actions::SetOnQuit, _, cx| {
            this.set_on_quit(a.0, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::OpenProject, window, cx| {
            if this
                .picker()
                .is_some_and(|p| p.kind == PickerKind::Projects)
            {
                this.close_picker(window, cx);
            } else {
                this.open_project_picker(window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::OpenFolder, window, cx| {
            this.open_folder_dialog(window, cx);
        }))
        .on_action(cx.listener(|this, _: &actions::OpenJump, window, cx| {
            if this.picker().is_some_and(|p| p.kind == PickerKind::Jump) {
                this.close_picker(window, cx);
            } else {
                this.open_jump_palette(window, cx);
            }
        }))
        .on_action(cx.listener(|this, _: &actions::GoHome, window, cx| {
            let home = this.sessions.read(cx).home_workspace().map(|w| w.id);
            match home {
                Some(home) => this.switch_workspace(home, window, cx),
                None => this.set_notice("No Home workspace yet", cx),
            }
        }))
        .on_action(cx.listener(|this, a: &actions::OpenWorkspace, window, cx| {
            if let Ok(id) = a.0.parse() {
                this.switch_workspace(id, window, cx);
            }
        }))
        .on_action(
            cx.listener(|this, a: &actions::ShowWorkspaceMenu, window, cx| {
                if let Ok(id) = a.0.workspace.parse() {
                    this.show_workspace_menu(id, a.0.position, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, a: &actions::ShowSortMenu, window, cx| {
            this.show_sort_menu(a.0, window, cx);
        }))
        .on_action(cx.listener(|this, a: &actions::SetWorkspaceSort, _, cx| {
            this.set_workspace_sort(a.0, cx);
        }))
        .on_action(
            cx.listener(|this, a: &actions::MoveWorkspaceOnto, window, cx| {
                if let (Ok(dragged), Ok(target)) = (a.0.dragged.parse(), a.0.target.parse()) {
                    this.drop_workspace(dragged, target, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::PinActiveWorkspace, window, cx| {
                if let Some(info) = this.active_workspace_info(cx) {
                    this.pin_workspace(info.id, !info.pinned, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::RenameActiveWorkspace, window, cx| {
                if let Some(id) = this.active_workspace() {
                    this.rename_workspace(id, window, cx);
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::AddFolderToWorkspace, window, cx| {
                match this.active_workspace_info(cx) {
                    Some(info) if info.kind == heeler_protocol::messages::WorkspaceKind::Home => {
                        this.set_notice("Home has one folder; change it in daemon.toml", cx);
                    }
                    Some(info) => this.add_workspace_folder(info.id, window, cx),
                    None => {}
                }
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::CloseActiveWorkspace, window, cx| {
                if let Some(id) = this.active_workspace() {
                    this.close_workspace(id, window, cx);
                }
            }),
        )
        .on_action(cx.listener(|this, _: &actions::RevealWorkspace, _, cx| {
            if let Some(id) = this.active_workspace() {
                this.reveal_workspace(id, cx);
            }
        }))
        .on_action(
            cx.listener(|this, _: &actions::ToggleHideEmptyWorkspaces, _, cx| {
                this.toggle_workspace_filter("hide_empty", cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ToggleHideSleepingWorkspaces, _, cx| {
                this.toggle_workspace_filter("hide_sleeping", cx);
            }),
        )
        .on_action(
            cx.listener(|this, _: &actions::ToggleHideCliWorkspaces, _, cx| {
                this.toggle_workspace_filter("hide_cli_created", cx);
            }),
        )
    }

    // ---- title bar ---------------------------------------------------------------------------

    fn render_titlebar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let left_visible = self.state.docks.left.visible;
        let overview_active = self.state.preset == LayoutPreset::OVERVIEW;
        let overview_hint =
            key_hint(window, &actions::ToggleOverview).unwrap_or_else(|| "⌘⇧O".into());
        let preset_name = preset_label(&self.state.preset);
        let hover = theme.hover;

        let preset_button = div()
            .id("titlebar-preset")
            .px(px(8.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .border_1()
            .border_color(theme.border)
            .text_size(px(11.0))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                this.show_preset_menu(event.position(), window, cx);
            }))
            .child(format!("Layout: {preset_name} ▾"));

        let accent = theme.accent;
        let overview_button = div()
            .id("titlebar-overview")
            .px(px(8.0))
            .h(px(22.0))
            .flex()
            .items_center()
            .rounded(px(5.0))
            .border_1()
            .border_color(if overview_active {
                accent
            } else {
                theme.border
            })
            .text_size(px(11.0))
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .when(overview_active, move |s| s.bg(accent.opacity(0.18)))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(|this, _, _, cx| this.toggle_overview(cx)))
            .child(format!("⊞ Overview {overview_hint}"));

        div()
            .id("titlebar")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.0))
            .h(px(TITLEBAR_HEIGHT))
            .w_full()
            .pl(px(if left_visible {
                12.0
            } else {
                TRAFFIC_LIGHT_INSET
            }))
            .pr(px(10.0))
            .bg(theme.surface)
            .border_b_1()
            .border_color(theme.border)
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .justify_center()
                    .truncate()
                    .text_size(px(tokens.font_size))
                    .text_color(theme.muted)
                    .child(self.active_title(cx)),
            )
            .child(preset_button)
            .child(overview_button)
            .child(probe(&self.regions, Region::Titlebar))
            .into_any_element()
    }

    // ---- docks -------------------------------------------------------------------------------

    fn render_dock(
        &mut self,
        pos: DockPosition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let dock = self.state.docks.get(pos).clone();
        if !dock.visible {
            return None;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let is_left = pos == DockPosition::Left;
        let hover = theme.hover;
        let accent = theme.accent;

        let tabs: Vec<AnyElement> = dock
            .panels
            .iter()
            .enumerate()
            .map(|(index, panel)| {
                let active = dock.active.as_ref() == Some(panel);
                let (icon, title) = self.item_icon_and_title(
                    &ItemRef::Panel {
                        panel: panel.clone(),
                    },
                    cx,
                );
                let source = ItemSource::Dock {
                    pos,
                    panel: panel.clone(),
                };
                let dragged = DraggedItem {
                    source,
                    item: ItemRef::Panel {
                        panel: panel.clone(),
                    },
                    label: title.clone(),
                };
                let click_panel = panel.clone();
                let menu_panel = panel.clone();
                div()
                    .id(id(format!("dock-tab-{pos:?}-{panel}")))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .px(px(tokens.spacing * 2.0))
                    .h_full()
                    .cursor_pointer()
                    .text_color(if active { theme.fg } else { theme.muted })
                    .when(active, |s| s.bg(theme.active_tab))
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let panel = click_panel.clone();
                        this.mutate(cx, |s| {
                            let _ = s.set_dock_active(pos, &panel);
                            s.focus_dock(pos);
                        });
                        this.focus_root(window, cx);
                    }))
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let entries = this.panel_menu(&menu_panel);
                            this.open_menu(event.position, entries, window, cx);
                        }),
                    )
                    .on_drag(dragged, |dragged: &DraggedItem, _, _, cx| {
                        let style = super::style::UiStyle::get(cx);
                        let label = dragged.label.clone();
                        cx.new(|_| DragPreview { label, style })
                    })
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.border_l_2().border_color(accent))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(d, DropTarget::Dock { pos, index }, window, cx);
                    }))
                    .child(icon)
                    .child(title)
                    .child(probe(&self.regions, Region::DockTab(pos, index)))
                    .into_any_element()
            })
            .collect();

        let append_index = dock.panels.len();
        let body: AnyElement = match dock
            .active
            .as_ref()
            .and_then(|a| self.panel_view(a, window, cx))
        {
            Some(view) => div().size_full().child(view).into_any_element(),
            None => div()
                .size_full()
                .p(px(tokens.spacing * 3.0))
                .text_color(theme.muted)
                .child("Empty dock. Drag a panel here, or press ⌘K to open one.")
                .into_any_element(),
        };

        let handle = {
            let base = div()
                .id(id(format!("dock-resize-{pos:?}")))
                .absolute()
                .hover(move |s| s.bg(accent.opacity(0.35)))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.start_dock_resize(pos, event.position);
                        cx.stop_propagation();
                    }),
                );
            match pos {
                DockPosition::Left => base
                    .right_0()
                    .top_0()
                    .h_full()
                    .w(px(HANDLE_THICKNESS))
                    .cursor_col_resize(),
                DockPosition::Right => base
                    .left_0()
                    .top_0()
                    .h_full()
                    .w(px(HANDLE_THICKNESS))
                    .cursor_col_resize(),
                DockPosition::Bottom => base
                    .top_0()
                    .left_0()
                    .w_full()
                    .h(px(HANDLE_THICKNESS))
                    .cursor_row_resize(),
            }
        };

        let root = div()
            .id(id(format!("dock-{pos:?}")))
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .bg(if is_left {
                theme.sidebar
            } else {
                theme.surface
            })
            .border_color(theme.border)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| {
                    this.mutate(cx, |s| s.focus_dock(pos));
                    this.focus_root(window, cx);
                }),
            );
        let root = match pos {
            DockPosition::Left => root.w(px(dock.size)).border_r_1(),
            DockPosition::Right => root.w(px(dock.size)).border_l_1(),
            DockPosition::Bottom => root.h(px(dock.size)).w_full().border_t_1(),
        };

        Some(
            root.when(is_left, |s| {
                // Inset for the traffic lights; also a window drag handle.
                s.child(
                    div()
                        .h(px(TITLEBAR_HEIGHT))
                        .flex_none()
                        .w_full()
                        .on_mouse_down(MouseButton::Left, |event, window, cx| {
                            if event.click_count == 2 {
                                window.titlebar_double_click();
                            } else {
                                window.start_window_move();
                            }
                            cx.stop_propagation();
                        }),
                )
            })
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(tokens.tab_height))
                    .w_full()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_size(px(tokens.font_size))
                    .children(tabs)
                    .child(
                        div()
                            .id(id(format!("dock-tab-filler-{pos:?}")))
                            .flex_1()
                            .h_full()
                            .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.18)))
                            .can_drop(|any, _, _| {
                                any.downcast_ref::<DraggedItem>()
                                    .is_some_and(|d| d.item.as_panel().is_some())
                            })
                            .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                                this.drop_item(
                                    d,
                                    DropTarget::Dock {
                                        pos,
                                        index: append_index,
                                    },
                                    window,
                                    cx,
                                );
                            })),
                    ),
            )
            .child(
                div()
                    .id(id(format!("dock-body-{pos:?}")))
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.10)))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(
                            d,
                            DropTarget::Dock {
                                pos,
                                index: append_index,
                            },
                            window,
                            cx,
                        );
                    }))
                    .child(body),
            )
            .child(handle)
            .child(probe(&self.regions, Region::Dock(pos)))
            .into_any_element(),
        )
    }

    /// While a tab/panel is dragged, hidden docks expose a thin drop strip at their window edge.
    fn render_hidden_dock_targets(&mut self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        if !cx.has_active_drag() {
            return Vec::new();
        }
        let theme = self.theme;
        let status_h = self.density.ui().status_bar;
        let accent = theme.accent;
        DockPosition::ALL
            .into_iter()
            .filter(|pos| !self.state.docks.get(*pos).visible)
            .map(|pos| {
                let index = self.state.docks.get(pos).panels.len();
                let base = div()
                    .id(id(format!("hidden-dock-drop-{pos:?}")))
                    .absolute()
                    .bg(accent.opacity(0.10))
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.35)))
                    .can_drop(|any, _, _| {
                        any.downcast_ref::<DraggedItem>()
                            .is_some_and(|d| d.item.as_panel().is_some())
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(d, DropTarget::Dock { pos, index }, window, cx);
                    }));
                match pos {
                    DockPosition::Left => base
                        .left_0()
                        .top(px(TITLEBAR_HEIGHT))
                        .bottom(px(status_h))
                        .w(px(28.0)),
                    DockPosition::Right => base
                        .right_0()
                        .top(px(TITLEBAR_HEIGHT))
                        .bottom(px(status_h))
                        .w(px(28.0)),
                    DockPosition::Bottom => {
                        base.left_0().right_0().bottom(px(status_h)).h(px(28.0))
                    }
                }
                .into_any_element()
            })
            .collect()
    }

    // ---- center ------------------------------------------------------------------------------

    fn render_center(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let zoomed = self
            .state
            .zoomed
            .and_then(|z| self.state.center.pane(z).cloned());
        let body = match zoomed {
            Some(pane) => self.render_pane(&pane, window, cx),
            None => {
                let tree = self.state.center.clone();
                self.render_tree(&tree, Vec::new(), window, cx)
            }
        };
        div()
            .id("center")
            .flex_1()
            .min_h_0()
            .min_w_0()
            .bg(theme.surface)
            .child(body)
            .child(probe(&self.regions, Region::Center))
            .into_any_element()
    }

    fn render_tree(
        &mut self,
        tree: &PaneTree,
        path: Vec<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match tree {
            PaneTree::Pane(pane) => self.render_pane(pane, window, cx),
            PaneTree::Split {
                axis,
                ratios,
                children,
            } => {
                let horizontal = *axis == Axis::Horizontal;
                let bounds_cell: Rc<Cell<Bounds<Pixels>>> = Rc::new(Cell::new(Bounds::default()));
                let accent = self.theme.accent;
                let mut container = div()
                    .relative()
                    .size_full()
                    .flex()
                    .when(!horizontal, |s| s.flex_col())
                    .child({
                        // Records the container's painted bounds so a divider drag can convert
                        // mouse pixels to a ratio.
                        let cell = Rc::clone(&bounds_cell);
                        canvas(move |bounds, _, _| cell.set(bounds), |_, _, _, _| {})
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                    });
                for (index, (child, ratio)) in children.iter().zip(ratios).enumerate() {
                    let mut child_path = path.clone();
                    child_path.push(index);
                    let element = self.render_tree(child, child_path, window, cx);
                    container = container.child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_basis(px(0.0))
                            .flex_grow(*ratio)
                            .flex_shrink(1.0)
                            .min_w_0()
                            .min_h_0()
                            .overflow_hidden()
                            .child(div().flex_1().min_h_0().min_w_0().child(element)),
                    );
                }
                let mut cumulative = 0.0;
                for (divider, ratio) in ratios
                    .iter()
                    .take(ratios.len().saturating_sub(1))
                    .enumerate()
                {
                    cumulative += *ratio;
                    let cell = Rc::clone(&bounds_cell);
                    let path = path.clone();
                    let handle = div()
                        .id(id(format!("splitter-{path:?}-{divider}")))
                        .absolute()
                        .hover(move |s| s.bg(accent.opacity(0.35)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                                let bounds = cell.get();
                                let (origin, extent) = if horizontal {
                                    (f32::from(bounds.origin.x), f32::from(bounds.size.width))
                                } else {
                                    (f32::from(bounds.origin.y), f32::from(bounds.size.height))
                                };
                                this.resizing = Some(Resizing::Split {
                                    path: path.clone(),
                                    divider,
                                    horizontal,
                                    origin,
                                    extent,
                                });
                                cx.stop_propagation();
                            }),
                        );
                    container = container.child(if horizontal {
                        handle
                            .left(relative(cumulative))
                            .ml(px(-HANDLE_THICKNESS / 2.0))
                            .top_0()
                            .h_full()
                            .w(px(HANDLE_THICKNESS))
                            .cursor_col_resize()
                    } else {
                        handle
                            .top(relative(cumulative))
                            .mt(px(-HANDLE_THICKNESS / 2.0))
                            .left_0()
                            .w_full()
                            .h(px(HANDLE_THICKNESS))
                            .cursor_row_resize()
                    });
                }
                container.into_any_element()
            }
        }
    }

    fn render_pane(
        &mut self,
        pane: &Pane,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let accent = theme.accent;
        let pane_id = pane.id;
        let multiple = self.state.panes().len() > 1;
        let focused = self.state.focus == FocusTarget::Pane(pane_id);
        let dragging = cx.has_active_drag();

        let tabs: Vec<AnyElement> = pane
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                self.render_tab(pane_id, index, item, index == pane.active, focused, cx)
            })
            .collect();

        let strip = div()
            .flex()
            .flex_none()
            .items_center()
            .h(px(tokens.tab_height))
            .w_full()
            .overflow_hidden()
            .border_b_1()
            .border_color(theme.border)
            .children(tabs)
            .child(
                div()
                    .id(id(format!("pane-strip-filler-{pane_id}")))
                    .flex_1()
                    .h_full()
                    .drag_over::<DraggedItem>(move |s, _, _, _| s.bg(accent.opacity(0.18)))
                    .on_drop({
                        let append = pane.items.len();
                        cx.listener(move |this, d: &DraggedItem, window, cx| {
                            this.drop_item(
                                d,
                                DropTarget::Pane {
                                    pane: pane_id,
                                    index: append,
                                },
                                window,
                                cx,
                            );
                        })
                    })
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                            let entries = vec![
                                Workbench::entry("Split Right", move |this, window, cx| {
                                    this.split_pane_with_terminal(pane_id, Edge::Right, window, cx);
                                }),
                                Workbench::entry("Split Down", move |this, window, cx| {
                                    this.split_pane_with_terminal(
                                        pane_id,
                                        Edge::Bottom,
                                        window,
                                        cx,
                                    );
                                }),
                            ];
                            this.open_menu(event.position, entries, window, cx);
                        }),
                    ),
            );

        let content = match pane.active_item() {
            Some(item) => self.render_item(pane_id, item, window, cx),
            None => div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.muted)
                .child("No open items — ⌘T for a new tab, ⌘K for commands")
                .into_any_element(),
        };

        let zones: Vec<AnyElement> = if dragging {
            [
                (
                    "top",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Top,
                    },
                    (0.0, 0.0, 1.0, 0.25),
                ),
                (
                    "bottom",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Bottom,
                    },
                    (0.0, 0.75, 1.0, 0.25),
                ),
                (
                    "left",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Left,
                    },
                    (0.0, 0.25, 0.25, 0.5),
                ),
                (
                    "right",
                    DropTarget::Split {
                        pane: pane_id,
                        edge: Edge::Right,
                    },
                    (0.75, 0.25, 0.25, 0.5),
                ),
                (
                    "center",
                    DropTarget::Pane {
                        pane: pane_id,
                        index: pane.items.len(),
                    },
                    (0.25, 0.25, 0.5, 0.5),
                ),
            ]
            .into_iter()
            .map(|(name, target, (l, t, w, h))| {
                div()
                    .id(id(format!("drop-{pane_id}-{name}")))
                    .absolute()
                    .left(relative(l))
                    .top(relative(t))
                    .w(relative(w))
                    .h(relative(h))
                    .drag_over::<DraggedItem>(move |s, _, _, _| {
                        s.bg(accent.opacity(0.25)).border_2().border_color(accent)
                    })
                    .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                        this.drop_item(d, target.clone(), window, cx);
                    }))
                    .into_any_element()
            })
            .collect()
        } else {
            Vec::new()
        };

        div()
            .id(id(format!("pane-{pane_id}")))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.surface)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, window, cx| this.focus_pane(pane_id, window, cx)),
            )
            .child(probe(&self.regions, Region::Pane(pane_id)))
            .child(strip)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(div().size_full().child(content))
                    .children(zones),
            )
            .when(focused && multiple, move |s| {
                s.child(
                    div()
                        .absolute()
                        .inset_0()
                        .border_1()
                        .border_color(accent.opacity(0.8)),
                )
            })
            .into_any_element()
    }

    fn render_tab(
        &mut self,
        pane_id: PaneId,
        index: usize,
        item: &ItemRef,
        active: bool,
        pane_focused: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let accent = theme.accent;
        let hover = theme.hover;
        let (icon, title) = self.item_icon_and_title(item, cx);
        let badge = session_of_item(item).and_then(|id| {
            let badge = self.sessions.read(cx).agent_badge(id);
            super::panels::agent_badge(
                &theme,
                &badge,
                super::view::BadgeSite::Tab,
                id,
                &self.regions,
                cx,
            )
        });
        let attention = session_of_item(item).and_then(|id| {
            let store = self.sessions.read(cx);
            store
                .session(id)
                .map(|info| store.presentation(info))
                .filter(|p| p.needs_you)
        });
        let dragged = DraggedItem {
            source: ItemSource::Pane {
                pane: pane_id,
                index,
            },
            item: item.clone(),
            label: title.clone(),
        };
        div()
            .id(id(format!("tab-{pane_id}-{index}")))
            .group("tab")
            .flex()
            .flex_none()
            .items_center()
            .gap(px(tokens.spacing))
            .pl(px(tokens.spacing * 2.0))
            .pr(px(tokens.spacing))
            .h_full()
            .cursor_pointer()
            .text_color(if active { theme.fg } else { theme.muted })
            .when(active, |s| {
                s.bg(theme.active_tab)
                    .border_b_2()
                    .border_color(if pane_focused { accent } else { theme.border })
            })
            .hover(move |s| s.bg(hover))
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                this.try_mutate(cx, |s| {
                    s.set_active_item(pane_id, index)?;
                    s.focus_pane(pane_id)
                });
                if event.click_count() == 2 {
                    this.mutate(cx, |s| s.toggle_zoom());
                }
                this.focus_root(window, cx);
            }))
            .on_aux_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                if event.is_middle_click() {
                    this.try_mutate(cx, |s| s.close_item(pane_id, index));
                }
            }))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    let entries = this.tab_session_menu(pane_id, index);
                    this.open_menu(event.position, entries, window, cx);
                }),
            )
            .on_drag(dragged, |dragged: &DraggedItem, _, _, cx| {
                let style = super::style::UiStyle::get(cx);
                let label = dragged.label.clone();
                cx.new(|_| DragPreview { label, style })
            })
            .drag_over::<DraggedItem>(move |s, _, _, _| s.border_l_2().border_color(accent))
            .on_drop(cx.listener(move |this, d: &DraggedItem, window, cx| {
                this.drop_item(
                    d,
                    DropTarget::Pane {
                        pane: pane_id,
                        index,
                    },
                    window,
                    cx,
                );
            }))
            .child(icon)
            .child(title)
            .children(badge)
            .children(attention.map(|p| {
                div()
                    .flex_none()
                    .size(px(7.0))
                    .rounded_full()
                    .bg(super::panels::tone_color(&theme, p.tone))
            }))
            .child(
                div()
                    .id(id(format!("tab-close-{pane_id}-{index}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(tokens.font_size + 4.0))
                    .rounded(px(3.0))
                    .text_color(theme.muted)
                    .hover(move |s| s.bg(hover).text_color(theme.fg))
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.try_mutate(cx, |s| s.close_item(pane_id, index));
                        cx.stop_propagation();
                    }))
                    .child("×"),
            )
            .child(probe(&self.regions, Region::Tab(pane_id, index)))
            .into_any_element()
    }

    fn render_item(
        &mut self,
        pane: PaneId,
        item: &ItemRef,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        match item {
            ItemRef::Panel { panel } => match self.panel_view(panel, window, cx) {
                Some(view) => div().size_full().child(view).into_any_element(),
                None => self.placeholder_body(format!("Panel `{panel}` is not registered"), cx),
            },
            ItemRef::Placeholder { title } => div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(tokens.spacing))
                .child(
                    div()
                        .text_size(px(tokens.font_size + 4.0))
                        .child(title.clone()),
                )
                .child(div().text_color(theme.muted).child(
                    "Open a session from the sessions panel, or press ⌘T for a new terminal.",
                ))
                .into_any_element(),
            ItemRef::Terminal { session } => self.render_terminal_item(pane, session, window, cx),
            ItemRef::Chat { session } => self.render_chat_item(pane, session, cx),
        }
    }

    pub(crate) fn placeholder_body(&self, text: String, _cx: &mut Context<Self>) -> AnyElement {
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .text_color(self.theme.muted)
            .child(text)
            .into_any_element()
    }

    // ---- status bar --------------------------------------------------------------------------

    fn render_status_bar(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = self.theme;
        let tokens = self.density.ui();
        let data = {
            let store = self.sessions.read(cx);
            StatusData {
                connection: store.connection_state(),
                connection_line: self.daemon_line(cx),
                epoch: store.state().epoch.map(|e| e.to_string()),
                sessions: store.sessions().len(),
                workspace: store.active_workspace().map(|w| w.name.clone()),
                needs_you: store.needs_you().len(),
                approvals: store.approvals().len(),
                terminal_theme: heeler_settings::themes::resolve(
                    &self.terminal_settings.theme,
                    super::view::system_appearance(self.appearance),
                )
                .name
                .clone(),
            }
        };
        let ctx = StatusContext {
            theme: &theme,
            density: self.density,
            palette_hint: key_hint(window, &actions::TogglePalette).unwrap_or_else(|| "⌘K".into()),
            attention_hint: key_hint(window, &actions::JumpToNextAttention).unwrap_or_default(),
            data: &data,
        };
        let status_bar = self.status_bar();
        let render_side = |ids: &[String]| -> Vec<AnyElement> {
            ids.iter()
                .filter_map(|item| self.status_items.get(item))
                .map(|spec| {
                    div()
                        .relative()
                        .child((spec.render)(&ctx))
                        .child(probe(&self.regions, Region::StatusItem(spec.id)))
                        .into_any_element()
                })
                .collect()
        };
        let left = render_side(&status_bar.left);
        let right = render_side(&status_bar.right);
        div()
            .id("status-bar")
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(tokens.status_bar))
            .w_full()
            .px(px(tokens.spacing * 2.0))
            .bg(theme.surface)
            .border_t_1()
            .border_color(theme.border)
            .text_size(px(tokens.font_size - 1.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 3.0))
                    .children(left)
                    .children(
                        self.notice
                            .clone()
                            .map(|n| div().text_color(theme.accent).child(n)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing * 2.0))
                    .children(right),
            )
            .child(probe(&self.regions, Region::StatusBar))
            .into_any_element()
    }

    // ---- palette -----------------------------------------------------------------------------

    fn render_palette(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let palette = self.palette.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        const VISIBLE_ROWS: usize = 12;
        let start = palette.selected.saturating_sub(VISIBLE_ROWS - 1);
        let rows: Vec<AnyElement> = palette
            .matches
            .iter()
            .enumerate()
            .skip(start)
            .take(VISIBLE_ROWS)
            .filter_map(|(row, command_index)| {
                let command = self.commands.get(*command_index)?;
                let selected = row == palette.selected;
                let hint = key_hint(window, &*command.hint).unwrap_or_default();
                let hover = theme.hover;
                Some(
                    div()
                        .id(id(format!("palette-row-{row}")))
                        .flex()
                        .items_center()
                        .justify_between()
                        .h(px(tokens.sidebar_row))
                        .px(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius - 2.0))
                        .cursor_pointer()
                        .when(selected, |s| s.bg(theme.accent.opacity(0.25)))
                        .hover(move |s| s.bg(hover))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.palette_confirm(Some(row), window, cx);
                        }))
                        .child(command.title.clone())
                        .child(div().text_color(theme.muted).child(hint))
                        .into_any_element(),
                )
            })
            .collect();
        let empty = palette.matches.is_empty();

        Some(
            div()
                .id("palette-backdrop")
                .absolute()
                .inset_0()
                .flex()
                .justify_center()
                .items_start()
                .pt(px(80.0))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_palette(window, cx)),
                )
                .child(
                    div()
                        .id("palette")
                        .key_context("Palette")
                        .track_focus(&self.palette_focus)
                        .w(px(560.0))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .p(px(tokens.spacing * 2.0))
                        .rounded(px(tokens.radius + 4.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                            let keystroke = &event.keystroke;
                            if keystroke.key == "backspace" {
                                this.palette_backspace(cx);
                            } else if !keystroke.modifiers.platform
                                && !keystroke.modifiers.control
                                && !keystroke.modifiers.function
                                && let Some(text) = &keystroke.key_char
                            {
                                this.palette_type(text, cx);
                            }
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectNext, _, cx| {
                            this.palette_select(true, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::SelectPrev, _, cx| {
                            this.palette_select(false, cx);
                        }))
                        .on_action(cx.listener(|this, _: &palette::Confirm, window, cx| {
                            this.palette_confirm(None, window, cx);
                        }))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .h(px(tokens.sidebar_row + 6.0))
                                .px(px(tokens.spacing * 2.0))
                                .border_b_1()
                                .border_color(theme.border)
                                .child(div().text_color(theme.muted).child("› "))
                                .child(palette.query.clone())
                                .child(
                                    div()
                                        .w(px(1.0))
                                        .h(px(tokens.font_size + 2.0))
                                        .bg(theme.accent),
                                ),
                        )
                        .children(rows)
                        .when(empty, |s| {
                            s.child(
                                div()
                                    .px(px(tokens.spacing * 2.0))
                                    .text_color(theme.muted)
                                    .child("No matching commands"),
                            )
                        }),
                )
                .into_any_element(),
        )
    }

    // ---- context menu ------------------------------------------------------------------------

    fn render_context_menu(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let ContextMenu { position, entries } = self.context_menu.clone()?;
        let theme = self.theme;
        let tokens = self.density.ui();
        let hover = theme.hover;
        let rows: Vec<AnyElement> = entries
            .into_iter()
            .enumerate()
            .map(|(i, entry)| {
                let disabled = entry.disabled;
                div()
                    .id(id(format!("menu-row-{i}")))
                    .flex()
                    .items_center()
                    .gap(px(tokens.spacing))
                    .h(px(tokens.sidebar_row))
                    .px(px(tokens.spacing * 2.0))
                    .rounded(px(tokens.radius - 2.0))
                    .when(disabled, |s| s.text_color(theme.muted))
                    .when(!disabled, |s| {
                        s.cursor_pointer().hover(move |s| s.bg(hover))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.run_menu_entry(i, window, cx);
                    }))
                    .child(
                        div()
                            .w(px(12.0))
                            .child(if entry.checked { "✓" } else { "" }),
                    )
                    .child(entry.label)
                    .into_any_element()
            })
            .collect();
        Some(
            deferred(
                anchored()
                    .position(point(position.x, position.y))
                    .snap_to_window_with_margin(px(8.0))
                    .child(
                        div()
                            .id("context-menu")
                            .key_context("ContextMenu")
                            .track_focus(&self.menu_focus)
                            .occlude()
                            .min_w(px(190.0))
                            .flex()
                            .flex_col()
                            .p(px(tokens.spacing))
                            .rounded(px(tokens.radius))
                            .bg(theme.overlay)
                            .border_1()
                            .border_color(theme.border)
                            .shadow_lg()
                            .text_size(px(tokens.font_size))
                            .on_mouse_down_out(
                                cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                            )
                            .children(rows),
                    ),
            )
            .priority(10)
            .into_any_element(),
        )
    }
}
