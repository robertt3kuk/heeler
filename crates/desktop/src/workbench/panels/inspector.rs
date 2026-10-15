//! The `inspector` panel: the permission requests of every agent chat (answered here or in the
//! chat), and session info for the focused item; the changes tab arrives with M7.

use crate::theme::DensityExt as _;
use gpui::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::*, px,
};
use hutan_core::SessionKind;
use hutan_protocol::blocks::BlockState;
use hutan_protocol::messages::SessionInfo;
use hutan_terminal_view::blocks::{self as block_ui, Tone};

use super::common::{age_label, format_utc, panel_header, state_dot, tone_color};
use crate::settings::InspectorPanelSettings;
use crate::workbench::actions::{BlockRef, JumpToBlock};
use crate::workbench::registry::PanelEnv;
use crate::workbench::render::probe;
use crate::workbench::style::UiStyle;
use crate::workbench::view::Region;

/// Blocks the info tab lists.
const BLOCK_ROWS: usize = 20;

/// One command of the info tab's block list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRow {
    pub id: u64,
    pub command: String,
    pub state: BlockState,
    pub exit_code: Option<i32>,
    /// `1.2 s`; `None` while it runs or when no time is known.
    pub duration: Option<String>,
    pub cwd: Option<String>,
}

pub struct InspectorPanel {
    env: PanelEnv,
    /// The tab the user picked; falls back to the first configured tab when it is not shown.
    active: Option<String>,
}

impl std::fmt::Debug for InspectorPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InspectorPanel")
            .field("active", &self.active)
            .finish_non_exhaustive()
    }
}

fn tab_title(id: &str) -> &str {
    match id {
        "approvals" => "Approvals",
        "changes" => "Changes",
        "info" => "Info",
        other => other,
    }
}

fn kind_name(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Terminal => "Terminal",
        SessionKind::Harness => "Agent (harness)",
        SessionKind::Direct => "Chat (direct)",
    }
}

impl InspectorPanel {
    pub fn new(env: PanelEnv, cx: &mut Context<Self>) -> Self {
        cx.observe(&env.sessions, |_, _, cx| cx.notify()).detach();
        Self { env, active: None }
    }

    /// The last [`BLOCK_ROWS`] commands of the session's terminal, newest first (tests and
    /// rendering share this). Empty while the terminal is not attached or has no blocks.
    pub fn block_rows(&self, info: &SessionInfo, cx: &gpui::App) -> Vec<BlockRow> {
        let Some(terminal) = self.env.sessions.read(cx).attached(info.id) else {
            return Vec::new();
        };
        let shared = terminal.read(cx).shared().clone();
        let model = shared.read(cx).model();
        model
            .blocks()
            .iter()
            .rev()
            .filter(|block| block.state != BlockState::Prompt)
            .take(BLOCK_ROWS)
            .map(|block| BlockRow {
                id: block.id,
                command: block
                    .command
                    .as_deref()
                    .map(|c| c.lines().next().unwrap_or_default().trim().to_owned())
                    .unwrap_or_default(),
                state: block.state,
                exit_code: block.exit_code,
                duration: block_ui::duration_ms(block).map(block_ui::format_duration),
                cwd: block.cwd.as_ref().map(|c| c.display().to_string()),
            })
            .collect()
    }

    /// Label/value rows of the info tab for `info` (tests and rendering share this).
    pub fn info_rows(&self, info: &SessionInfo, cx: &gpui::App) -> Vec<(&'static str, String)> {
        let store = self.env.sessions.read(cx);
        let presentation = store.presentation(info);
        let state = match &info.state {
            hutan_core::SessionState::Exited { code, signal } => {
                let mut text = "exited".to_owned();
                if let Some(code) = code {
                    text.push_str(&format!(" with code {code}"));
                }
                if let Some(signal) = signal {
                    text.push_str(&format!(", signal {signal}"));
                }
                text
            }
            hutan_core::SessionState::Failed { reason } => format!("failed: {reason}"),
            _ => presentation.label.clone(),
        };
        let size = store
            .attached(info.id)
            .and_then(|terminal| {
                let shared = terminal.read(cx).shared().clone();
                let model = shared.read(cx).model();
                model
                    .is_ready()
                    .then(|| format!("{} × {}", model.cols(), model.screen_rows()))
            })
            .unwrap_or_else(|| "not attached".to_owned());
        let workspace = store
            .workspace(info.workspace_id)
            .map(|w| w.name.clone())
            .unwrap_or_default();
        vec![
            ("ID", info.id.to_string()),
            ("Kind", kind_name(info.kind).to_owned()),
            ("Title", info.title.clone()),
            ("Workspace", workspace),
            (
                "Command",
                if info.command.is_empty() {
                    "login shell".to_owned()
                } else {
                    info.command.join(" ")
                },
            ),
            (
                "Directory",
                info.cwd
                    .as_ref()
                    .map(|c| c.display().to_string())
                    .unwrap_or_default(),
            ),
            ("State", state),
            (
                "Terminal",
                info.terminal_id
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "none".to_owned()),
            ),
            ("Size", size),
            (
                "Created",
                format!(
                    "{} ({} ago)",
                    format_utc(info.created_at_ms),
                    age_label(info.created_at_ms)
                ),
            ),
        ]
    }
}

impl InspectorPanel {
    /// One pending request: what is asked, in which session, and a button per offered option.
    fn approval_row(
        &self,
        approval: &hutan_protocol::chat::ApprovalInfo,
        theme: &crate::theme::Theme,
        tokens: &crate::theme::Tokens,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let title = self
            .env
            .sessions
            .read(cx)
            .session(approval.session_id)
            .map_or_else(|| "a session that is gone".to_owned(), |s| s.title.clone());
        let session = approval.session_id.to_string();
        let hover = theme.hover;
        let ordinal = self
            .env
            .sessions
            .read(cx)
            .approvals()
            .iter()
            .position(|a| a.id == approval.id)
            .unwrap_or(0);
        let regions = self.env.regions.clone();
        let buttons = approval.options.iter().enumerate().map(|(number, option)| {
            let (id, option_id) = (approval.id, option.id.clone());
            div()
                .id(SharedString::from(format!(
                    "inspector-approve-{id}-{number}"
                )))
                .relative()
                .px(px(tokens.spacing * 2.0))
                .h(px(tokens.sidebar_row))
                .flex()
                .items_center()
                .rounded(px(tokens.radius - 1.0))
                .border_1()
                .border_color(theme.border)
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, _, _, cx| {
                    let task = this.env.sessions.update(cx, |store, cx| {
                        store.respond_approval(id, option_id.clone(), cx)
                    });
                    cx.spawn(async move |_, _| {
                        if let Err(error) = task.await {
                            tracing::warn!("could not answer a permission request: {error}");
                        }
                    })
                    .detach();
                }))
                .child(SharedString::from(format!(
                    "{} {}",
                    number + 1,
                    option.label
                )))
                .child(probe(
                    &regions,
                    Region::InspectorOption(ordinal * 8 + number),
                ))
        });
        div()
            .flex()
            .flex_col()
            .gap(px(tokens.spacing))
            .p(px(tokens.spacing * 2.0))
            .rounded(px(tokens.radius))
            .border_1()
            .border_color(theme.warning)
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child(SharedString::from(approval.title.clone())),
            )
            .child(
                div()
                    .id(SharedString::from(format!(
                        "inspector-open-{}",
                        approval.id
                    )))
                    .text_color(theme.accent)
                    .cursor_pointer()
                    .on_click(move |_, window, cx| {
                        window.dispatch_action(
                            Box::new(crate::workbench::actions::OpenSession(session.clone())),
                            cx,
                        );
                    })
                    .child(SharedString::from(format!("in {title} →"))),
            )
            .children(approval.detail.as_ref().map(|detail| {
                div()
                    .text_color(theme.muted)
                    .child(SharedString::from(detail.clone()))
            }))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(tokens.spacing))
                    .children(buttons),
            )
            .into_any_element()
    }
}

impl Render for InspectorPanel {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let style = UiStyle::get(cx);
        let (theme, tokens) = (style.theme, style.density.ui());
        let settings = self.env.settings.get::<InspectorPanelSettings>();
        let active = self
            .active
            .as_ref()
            .filter(|a| settings.tabs.contains(a))
            .or_else(|| settings.tabs.first())
            .cloned()
            .unwrap_or_default();
        let hover = theme.hover;
        let focused = {
            let store = self.env.sessions.read(cx);
            store
                .last_focused()
                .and_then(|id| store.session(id).cloned())
        };

        let tabs: Vec<AnyElement> = settings
            .tabs
            .iter()
            .map(|tab| {
                let selected = *tab == active;
                let id = tab.clone();
                div()
                    .id(SharedString::from(format!("inspector-tab-{tab}")))
                    .px(px(tokens.spacing * 2.0))
                    .h(px(tokens.sidebar_row))
                    .flex()
                    .items_center()
                    .cursor_pointer()
                    .text_color(if selected { theme.fg } else { theme.muted })
                    .when(selected, |s| {
                        s.font_weight(gpui::FontWeight::SEMIBOLD)
                            .border_b_2()
                            .border_color(theme.accent)
                    })
                    .hover(move |s| s.bg(hover))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.active = Some(id.clone());
                        cx.notify();
                    }))
                    .child(SharedString::from(tab_title(tab).to_owned()))
                    .into_any_element()
            })
            .collect();

        let note = |text: &str| -> AnyElement {
            div()
                .p(px(tokens.spacing * 3.0))
                .text_color(theme.muted)
                .child(SharedString::from(text.to_owned()))
                .into_any_element()
        };
        let body: AnyElement = match active.as_str() {
            "approvals" => {
                let approvals = self.env.sessions.read(cx).approvals().to_vec();
                if approvals.is_empty() {
                    note("No pending approvals.")
                } else {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing * 2.0))
                        .p(px(tokens.spacing * 3.0))
                        .children(
                            approvals
                                .iter()
                                .map(|approval| self.approval_row(approval, &theme, &tokens, cx)),
                        )
                        .into_any_element()
                }
            }
            "changes" => note("No changes to show. Worktree changes arrive with M7."),
            "info" => match &focused {
                None => note("Focus a session to see its details."),
                Some(info) => {
                    let presentation = self.env.sessions.read(cx).presentation(info);
                    let rows = self.info_rows(info, cx);
                    let block_rows = self.block_rows(info, cx);
                    let session = info.id.to_string();
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(tokens.spacing))
                        .p(px(tokens.spacing * 3.0))
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(tokens.spacing * 1.5))
                                .child(state_dot(&theme, &presentation))
                                .child(
                                    div()
                                        .font_weight(gpui::FontWeight::SEMIBOLD)
                                        .child(SharedString::from(info.title.clone())),
                                )
                                .child(
                                    div()
                                        .text_color(tone_color(&theme, presentation.tone))
                                        .child(SharedString::from(presentation.label.clone())),
                                ),
                        )
                        .children(rows.into_iter().map(|(label, value)| {
                            div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .text_size(px(tokens.section_font_size))
                                        .text_color(theme.muted)
                                        .child(SharedString::from(label.to_uppercase())),
                                )
                                .child(
                                    div()
                                        .id(SharedString::from(format!("info-{label}")))
                                        .child(SharedString::from(value)),
                                )
                        }))
                        .when(!block_rows.is_empty(), |s| {
                            s.child(
                                div()
                                    .mt(px(tokens.spacing * 2.0))
                                    .text_size(px(tokens.section_font_size))
                                    .text_color(theme.muted)
                                    .child("RECENT COMMANDS"),
                            )
                            .children(block_rows.into_iter().map(|row| {
                                let tone = match row.state {
                                    BlockState::Running => Tone::Neutral,
                                    _ => match row.exit_code {
                                        Some(0) => Tone::Success,
                                        Some(_) => Tone::Error,
                                        None => Tone::Neutral,
                                    },
                                };
                                let color = match tone {
                                    Tone::Success => theme.success,
                                    Tone::Error => theme.error,
                                    Tone::Neutral => theme.muted,
                                };
                                let status = match (row.state, row.exit_code) {
                                    (BlockState::Running, _) => "running".to_owned(),
                                    (_, Some(0)) => "✓".to_owned(),
                                    (_, Some(code)) => format!("✗ {code}"),
                                    (_, None) => "–".to_owned(),
                                };
                                let jump = BlockRef {
                                    session: session.clone(),
                                    block: row.id,
                                };
                                div()
                                    .id(SharedString::from(format!("inspector-block-{}", row.id)))
                                    .flex()
                                    .flex_col()
                                    .px(px(tokens.spacing))
                                    .py(px(tokens.spacing * 0.5))
                                    .rounded(px(tokens.radius - 2.0))
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(hover))
                                    .on_click(cx.listener(move |_, _, window, cx| {
                                        window.dispatch_action(
                                            Box::new(JumpToBlock(jump.clone())),
                                            cx,
                                        );
                                    }))
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap(px(tokens.spacing * 1.5))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .overflow_hidden()
                                                    .whitespace_nowrap()
                                                    .child(SharedString::from(row.command)),
                                            )
                                            .child(
                                                div()
                                                    .flex_none()
                                                    .text_color(color)
                                                    .child(SharedString::from(status)),
                                            )
                                            .children(row.duration.map(|d| {
                                                div()
                                                    .flex_none()
                                                    .text_color(theme.muted)
                                                    .child(SharedString::from(d))
                                            })),
                                    )
                                    .children(row.cwd.map(|cwd| {
                                        div()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .text_size(px(tokens.font_size - 1.0))
                                            .text_color(theme.muted)
                                            .child(SharedString::from(cwd))
                                    }))
                            }))
                        })
                        .into_any_element()
                }
            },
            other => note(&format!("Unknown inspector tab `{other}`.")),
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .text_size(px(tokens.font_size))
            .text_color(theme.fg)
            .child(panel_header("Inspector", "panels.inspector", cx))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .border_b_1()
                    .border_color(theme.border)
                    .children(tabs),
            )
            .child(
                div()
                    .id("inspector-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(body),
            )
    }
}
