//! The daemon popover opened from the `connection` status item: status fields, the controls
//! that the `Daemon` menu and the palette offer too (they all dispatch the same `daemon::*`
//! actions), and the keep-running switch bound to `app.keep_running`.

use std::time::Duration;

use gpui::{
    Action, AnyElement, Context, InteractiveElement, IntoElement, MouseButton, ParentElement,
    SharedString, StatefulInteractiveElement, Styled, div, prelude::*, px,
};

use super::actions::daemon;
use super::panels::StatusTone;
use super::render::probe;
use super::view::{Region, Workbench};
use crate::macos::LoginItemStatus;
use crate::macos::status_item::KEEP_RUNNING;
use crate::theme::DensityExt as _;

const WIDTH: f32 = 440.0;

/// `3d 4h`, `2h 5m`, `7m 12s`, `9s`.
fn uptime_text(uptime: Duration) -> String {
    let secs = uptime.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m {}s", secs / 60, secs % 60),
        3600..=86_399 => format!("{}h {}m", secs / 3600, secs % 3600 / 60),
        _ => format!("{}d {}h", secs / 86_400, secs % 86_400 / 3600),
    }
}

impl Workbench {
    pub(crate) fn render_daemon_popover(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.daemon.popover {
            return None;
        }
        let theme = self.theme;
        let tokens = self.density.ui();
        let connected = self.daemon_connected(cx);
        let busy = self.daemon.busy.is_some();
        let (line, tone) = self.daemon_line(cx);
        let dot = match tone {
            StatusTone::Ok => theme.success,
            StatusTone::Warn => theme.warning,
            StatusTone::Error => theme.error,
            StatusTone::Muted => theme.muted,
        };
        let (live, total) = {
            let sessions = self.sessions.read(cx).sessions();
            (
                sessions.iter().filter(|s| s.state.is_live()).count(),
                sessions.len(),
            )
        };
        let socket = self
            .connection(cx)
            .socket_path()
            .map(|p| p.display().to_string());
        let log = self
            .daemon
            .log_path
            .as_ref()
            .map(|p| p.display().to_string());
        let keep_running = self.keep_running_enabled();
        let keep_available = self.keep_running_available();
        let keep_status = self.keep_running_status();
        let hover = theme.hover;

        let mut fields: Vec<(&'static str, String)> = Vec::new();
        if let Some(status) = self.daemon.status.as_ref().filter(|_| connected) {
            let info = &status.info;
            fields.push((
                "Daemon",
                format!(
                    "hutand {} · protocol {}.{}",
                    info.daemon_version, info.protocol.major, info.protocol.minor
                ),
            ));
            fields.push(("PID", info.pid.to_string()));
            fields.push((
                "Uptime",
                uptime_text(Duration::from_millis(info.uptime_ms) + status.at.elapsed()),
            ));
            fields.push(("Sessions", format!("{live} live / {total} total")));
            fields.push(("Clients", info.clients.to_string()));
        } else if connected {
            fields.push(("Sessions", format!("{live} live / {total} total")));
        }
        if let Some(socket) = socket {
            fields.push(("Socket", socket));
        }
        if let Some(log) = log {
            fields.push(("Log", log));
        }

        let regions = self.regions.clone();
        let button = |id: &'static str, label: &'static str, action: Box<dyn Action>| {
            div()
                .id(id)
                .relative()
                .px(px(tokens.spacing * 3.0))
                .h(px(tokens.sidebar_row))
                .flex()
                .items_center()
                .rounded(px(tokens.radius - 1.0))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
                .child(label)
                .child(probe(&regions, Region::Popover(id)))
        };
        let mut actions = div().flex().flex_wrap().gap(px(tokens.spacing * 2.0));
        if connected && !busy {
            actions = actions
                .child(button(
                    "daemon-restart",
                    "Restart Daemon",
                    Box::new(daemon::Restart),
                ))
                .child(button("daemon-stop", "Stop Daemon", Box::new(daemon::Stop)));
        }
        if !connected && !busy {
            actions = actions.child(button(
                "daemon-start",
                "Start Daemon",
                Box::new(daemon::Start),
            ));
        }
        actions = actions.child(button(
            "daemon-logs",
            "Open Logs",
            Box::new(daemon::OpenLogs),
        ));

        // The keep-running switch: where it is, and what the system says about it.
        let switch = div()
            .id("daemon-keep-running")
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(tokens.spacing * 2.0))
            .px(px(tokens.spacing * 2.0))
            .py(px(tokens.spacing))
            .rounded(px(tokens.radius - 1.0))
            .border_1()
            .border_color(if keep_running && keep_available {
                theme.accent
            } else {
                theme.border
            })
            .when(keep_running && keep_available, |s| {
                s.bg(theme.accent.opacity(0.25))
            })
            .when(keep_available, |s| {
                s.cursor_pointer()
                    .hover(move |s| s.bg(hover))
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(Box::new(daemon::ToggleKeepRunning), cx);
                    })
            })
            .child(KEEP_RUNNING)
            .child(div().text_color(theme.muted).child(if keep_available {
                format!(
                    "{} · {}",
                    if keep_running { "on" } else { "off" },
                    keep_status.label()
                )
            } else {
                "available in Hutan.app".to_owned()
            }))
            .child(probe(&regions, Region::Popover("keep_running")));

        Some(
            div()
                .id("daemon-backdrop")
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| this.close_daemon_popover(window, cx)),
                )
                .child(
                    div()
                        .id("daemon-popover")
                        .key_context("DaemonPopover")
                        .track_focus(&self.daemon_focus)
                        .absolute()
                        .left(px(8.0))
                        .bottom(px(tokens.status_bar + 4.0))
                        .w(px(WIDTH))
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .p(px(tokens.spacing * 4.0))
                        .rounded(px(tokens.radius + 4.0))
                        .bg(theme.overlay)
                        .border_1()
                        .border_color(theme.border)
                        .shadow_lg()
                        .text_size(px(tokens.font_size))
                        .text_color(theme.fg)
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(tokens.spacing * 2.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(div().size(px(8.0)).rounded_full().bg(dot))
                                .child(SharedString::from(line)),
                        )
                        .when(self.daemon.stopped, |s| {
                            s.child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_between()
                                    .px(px(tokens.spacing * 2.0))
                                    .py(px(tokens.spacing))
                                    .rounded(px(tokens.radius - 1.0))
                                    .border_1()
                                    .border_color(theme.error)
                                    .child("Daemon stopped — Start it to reconnect")
                                    .when(!busy, |s| {
                                        s.child(button(
                                            "daemon-banner-start",
                                            "Start",
                                            Box::new(daemon::Start),
                                        ))
                                    }),
                            )
                        })
                        .child(div().flex().flex_col().gap(px(2.0)).children(
                            fields.into_iter().map(|(name, value)| {
                                div()
                                    .flex()
                                    .gap(px(tokens.spacing * 2.0))
                                    .child(
                                        div()
                                            .w(px(64.0))
                                            .flex_none()
                                            .text_color(theme.muted)
                                            .child(name),
                                    )
                                    .child(
                                        div().flex_1().min_w_0().child(SharedString::from(value)),
                                    )
                            }),
                        ))
                        .child(actions)
                        .child(switch)
                        .when(
                            keep_available && keep_status == LoginItemStatus::RequiresApproval,
                            |s| {
                                s.child(button(
                                    "daemon-login-items",
                                    "Open Login Items",
                                    Box::new(daemon::OpenLoginItems),
                                ))
                            },
                        ),
                )
                .into_any_element(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_reads_in_the_two_largest_units() {
        assert_eq!(uptime_text(Duration::from_secs(9)), "9s");
        assert_eq!(uptime_text(Duration::from_secs(432)), "7m 12s");
        assert_eq!(uptime_text(Duration::from_secs(7_500)), "2h 5m");
        assert_eq!(
            uptime_text(Duration::from_secs(3 * 86_400 + 4 * 3600)),
            "3d 4h"
        );
    }
}
