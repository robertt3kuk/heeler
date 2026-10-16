//! Workbench behavior around the Assistant panel (M16-B): ⌥Space / ⌘K open and focus it, a
//! history hit that is an assistant conversation shows it there, and "Send to agent" of a
//! terminal block becomes a structured block attachment when the target is an assistant.
//!
//! The panel is a registered panel like the others (`panels::assistant`); the workbench only
//! reveals it, hands it a session or a chip, and puts the keyboard in its composer. Nothing
//! here reads a terminal or a file: chips describe a source and the daemon resolves them when
//! the message is sent.

use gpui::{Context, Entity, Window};
use hutan_core::SessionId;

use super::chat::ContextChip;
use super::model::PanelId;
use super::panels::{AssistantPanel, ScopeKey};
use super::view::Workbench;

impl Workbench {
    /// `assistant.enabled`: off hides the panel and its commands and shortcuts.
    pub fn assistant_enabled(&self) -> bool {
        self.settings
            .get::<crate::settings::AssistantSettings>()
            .enabled
    }

    /// The setting changed: closes the panel, drops the commands and the key, and lets the
    /// global shortcut go (or brings them back).
    pub(crate) fn apply_assistant_enabled(&mut self, cx: &mut Context<Self>) {
        if !self.assistant_enabled() {
            let id = PanelId::new("assistant");
            self.try_mutate(cx, |s| s.close_panel(&id));
            self.panel_views.remove(&id);
        }
        self.commands = std::rc::Rc::new(super::actions::commands(
            &self.presets,
            &self.panels,
            self.assistant_enabled(),
        ));
        self.rebind_keys(cx);
        self.sync_hotkey(cx);
    }

    /// Reveals the Assistant panel (in its default dock when it is closed) and returns it.
    pub(crate) fn assistant_panel(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Entity<AssistantPanel>> {
        if !self.assistant_enabled() {
            return None;
        }
        self.open_panel("assistant", cx);
        let view = self.panel_view(&PanelId::new("assistant"), window, cx)?;
        view.downcast::<AssistantPanel>().ok()
    }

    /// ⌥Space and ⌘K "Assistant: Open / Focus": shows the panel with the assistant of its scope
    /// (the first use asks the daemon to open it) and puts the keyboard in its composer. No
    /// terminal is started and no vendor agent is run.
    pub fn open_assistant(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.assistant_enabled() {
            self.set_notice("The assistant is off (assistant.enabled)", cx);
            return;
        }
        let Some(panel) = self.assistant_panel(window, cx) else {
            self.set_notice("The Assistant panel is not available", cx);
            return;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            panel.ensure_open(cx);
            panel.focus_composer(cx);
        });
    }

    /// Whether `session` is an assistant conversation.
    pub(crate) fn is_assistant(&self, session: SessionId, cx: &gpui::App) -> bool {
        self.sessions
            .read(cx)
            .session(session)
            .and_then(|info| info.chat.as_ref())
            .is_some_and(|chat| chat.is_assistant())
    }

    /// Shows the assistant conversation `session` in the Assistant panel, in its own scope, with
    /// the keyboard in its composer. `false` (and nothing shown) when it is not one.
    pub fn show_assistant_session(
        &mut self,
        session: SessionId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.assistant_enabled() || !self.is_assistant(session, cx) {
            return false;
        }
        let Some(panel) = self.assistant_panel(window, cx) else {
            return false;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            let shown = panel.show_session(session, cx);
            if shown {
                panel.focus_composer(cx);
            }
            shown
        })
    }

    /// Attaches `chip` to the composer of the assistant `session` and shows it.
    pub(crate) fn attach_to_assistant(
        &mut self,
        session: SessionId,
        chip: ContextChip,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(panel) = self.assistant_panel(window, cx) else {
            return;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            if panel.attach_to_session(session, chip, cx) {
                panel.focus_composer(cx);
            }
        });
    }

    /// "Send to agent" of block `block` of terminal session `session` with no chat to send it to
    /// (or the Assistant picked from the menu): the block becomes a chip of the assistant of the
    /// terminal's workspace, which the daemon reads when the message is sent.
    pub(crate) fn send_block_to_assistant(
        &mut self,
        session: SessionId,
        block: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (terminal, title, key) = {
            let store = self.sessions.read(cx);
            let Some(info) = store.session(session) else {
                return;
            };
            let Some(terminal) = info.terminal_id else {
                return;
            };
            let key = store
                .workspace(info.workspace_id)
                .map_or(ScopeKey::Global, ScopeKey::of);
            (terminal, info.title.clone(), key)
        };
        let (command, exit_code) = self.block_facts(session, block, cx);
        let chip = ContextChip::block(&title, terminal, block, command.as_deref(), exit_code);
        let Some(panel) = self.assistant_panel(window, cx) else {
            return;
        };
        self.assistant_last = true;
        panel.update(cx, |panel, cx| {
            panel.attach_to_scope(key, chip, cx);
            panel.focus_composer(cx);
        });
    }

    /// What the terminal view knows of block `block`: its command and exit code.
    pub(crate) fn block_facts(
        &self,
        session: SessionId,
        block: u64,
        cx: &gpui::App,
    ) -> (Option<String>, Option<i32>) {
        self.terminal_view_of(session)
            .and_then(|view| {
                view.read(cx)
                    .model()
                    .blocks()
                    .get(block)
                    .map(|b| (b.command.clone(), b.exit_code))
            })
            .unwrap_or((None, None))
    }
}
