//! Workbench behavior around the agent monitor (M13-B): the ⌘K commands, and the context menu
//! of a node of an observation tree. The panel itself is `panels/agents.rs`.

use std::path::PathBuf;

use gpui::{ClipboardItem, Context, Pixels, Point, Window};
use staffy_core::SessionId;

use super::view::{MenuEntry, Workbench};

impl Workbench {
    /// "Agents: Show Monitor": reveals the panel.
    pub(crate) fn show_agent_monitor(&mut self, cx: &mut Context<Self>) {
        self.open_panel("agents", cx);
    }

    /// "Agents: Show Only Blocked": reveals the panel and lists only blocked agents (the
    /// `agents.only_blocked` setting; the panel's chip turns it off again).
    pub(crate) fn show_only_blocked_agents(&mut self, cx: &mut Context<Self>) {
        if let Err(err) = self
            .settings
            .write_user("agents.only_blocked", toml::Value::Boolean(true))
        {
            tracing::warn!("cannot save agents.only_blocked: {err}");
            self.set_notice("Could not save the setting", cx);
        }
        self.open_panel("agents", cx);
    }

    /// Reveals a node's transcript in Finder.
    pub(crate) fn reveal_transcript(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        (self.daemon.reveal.clone())(&path, cx);
    }

    pub(crate) fn copy_node_id(&mut self, node: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(node.to_owned()));
        self.set_notice("Copied the node id", cx);
    }

    /// The context menu of a node: Open session, Open transcript (only when the agent
    /// reported one), Copy node id.
    pub(crate) fn agent_node_menu(
        &self,
        session: SessionId,
        node: &str,
        cx: &gpui::App,
    ) -> Vec<MenuEntry> {
        let transcript = self
            .sessions
            .read(cx)
            .agent_tree(session)
            .and_then(|tree| tree.nodes.get(node))
            .and_then(|n| n.transcript_path.clone());
        let mut entries = vec![Self::entry("Open Session", move |this, window, cx| {
            this.open_session(session, window, cx);
        })];
        if let Some(path) = transcript {
            entries.push(Self::entry("Open Transcript", move |this, _, cx| {
                this.reveal_transcript(PathBuf::from(&path), cx);
            }));
        }
        let id = node.to_owned();
        entries.push(Self::entry("Copy Node ID", move |this, _, cx| {
            this.copy_node_id(&id, cx);
        }));
        entries
    }

    /// Opens the node context menu at `position` (the panel's right-click).
    pub(crate) fn show_agent_node_menu(
        &mut self,
        session: SessionId,
        node: &str,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.agent_node_menu(session, node, cx);
        self.open_menu(position, entries, window, cx);
    }
}
