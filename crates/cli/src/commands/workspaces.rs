//! Workspace commands: `ws ls|open|new|rename|close|move|pin|unpin|recent|use`, and the target
//! resolution `new` and `ls --ws` share.

use std::collections::HashMap;
use std::path::PathBuf;

use hutan_client::Client;
use hutan_core::{SessionState, WorkspaceId};
use hutan_protocol::messages::{
    AgentState, CreatedBy, Empty, RecentProjects, Request, SessionInfo, WorkspaceCreate,
    WorkspaceInfo, WorkspaceKind, WorkspaceList, WorkspaceMove, WorkspaceOpen, WorkspaceOpened,
    WorkspacePin, WorkspaceRecent, WorkspaceRef, WorkspaceRename,
};
use serde_json::Value;

use super::Ctx;
use super::sessions::{age_text, now_ms};
use crate::args::{WsCommand, WsLsArgs, WsMoveArgs, WsSort};
use crate::error::CliError;

/// Shortest workspace id prefix accepted as a target.
const MIN_PREFIX: usize = 4;

/// Environment variable naming the workspace `hutan new` uses when `--ws` is absent.
pub(super) const WORKSPACE_ENV: &str = "HUTAN_WORKSPACE";

impl Ctx {
    pub(super) async fn ws(&self, command: WsCommand) -> Result<(), CliError> {
        match command {
            WsCommand::Ls(args) => self.ws_ls(args).await,
            WsCommand::Open { path } => self.ws_open(path).await,
            WsCommand::New { name, roots } => self.ws_new(name, roots).await,
            WsCommand::Rename { id, name } => self.ws_rename(id, name).await,
            WsCommand::Close { id } => self.ws_close(id).await,
            WsCommand::Move(args) => self.ws_move(args).await,
            WsCommand::Pin { target } => self.ws_pin(&target, true).await,
            WsCommand::Unpin { target } => self.ws_pin(&target, false).await,
            WsCommand::Recent { limit } => self.ws_recent(limit).await,
            WsCommand::Use { target } => self.ws_use(&target).await,
        }
    }

    async fn ws_ls(&self, args: WsLsArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let mut list = self.workspace_list(&client).await?;
        // Only the attention order needs to look at the sessions.
        let attention = if args.sort == WsSort::Attention {
            let sessions = self.session_list(&client, None).await?;
            attention_counts(&sessions.sessions)
        } else {
            HashMap::new()
        };
        if !args.all {
            list.workspaces.retain(is_listed_by_default);
        }
        sort_workspaces(&mut list.workspaces, args.sort, &attention);
        self.print(&list, || workspace_table(&list.workspaces, now_ms()))
    }

    async fn ws_open(&self, path: PathBuf) -> Result<(), CliError> {
        // The daemon resolves paths from its own working directory, not ours.
        let path = std::path::absolute(path)?;
        let client = self.connect().await?;
        let opened: WorkspaceOpened = self
            .call(
                &client,
                Request::WorkspaceOpen(WorkspaceOpen {
                    path,
                    created_by: Some(CreatedBy::Cli),
                }),
            )
            .await?;
        self.print(&opened, || {
            let verb = if opened.created { "created" } else { "opened" };
            format!("{verb} {} ({})", opened.workspace.id, opened.workspace.name)
        })
    }

    async fn ws_new(&self, name: String, roots: Vec<PathBuf>) -> Result<(), CliError> {
        // The daemon resolves paths from its own working directory, not ours.
        let roots = roots
            .iter()
            .map(std::path::absolute)
            .collect::<Result<Vec<_>, _>>()?;
        let client = self.connect().await?;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspaceCreate(WorkspaceCreate {
                    name,
                    roots,
                    created_by: Some(CreatedBy::Cli),
                }),
            )
            .await?;
        self.print(&ws, || format!("created {} ({})", ws.id, ws.name))
    }

    async fn ws_rename(&self, id: WorkspaceId, name: String) -> Result<(), CliError> {
        let client = self.connect().await?;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspaceRename(WorkspaceRename {
                    workspace_id: id,
                    name,
                }),
            )
            .await?;
        self.print(&ws, || format!("renamed {} to {}", ws.id, ws.name))
    }

    async fn ws_close(&self, id: WorkspaceId) -> Result<(), CliError> {
        let client = self.connect().await?;
        let _: Value = self
            .call(
                &client,
                Request::WorkspaceClose(WorkspaceRef { workspace_id: id }),
            )
            .await?;
        self.print(
            &serde_json::json!({"workspace_id": id, "closed": true}),
            || format!("closed {id}"),
        )
    }

    async fn ws_move(&self, args: WsMoveArgs) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let moved = resolve_workspace(&all, &args.target)?;
        let before = match &args.before {
            Some(target) => Some(resolve_workspace(&all, target)?),
            None => None,
        };
        let list: WorkspaceList = self
            .call(
                &client,
                Request::WorkspaceMove(WorkspaceMove {
                    workspace_id: moved.id,
                    before: before.map(|w| w.id),
                }),
            )
            .await?;
        self.print(&list, || match before {
            Some(before) => format!("moved {} ({}) before {}", moved.id, moved.name, before.id),
            None => format!("moved {} ({}) to the end", moved.id, moved.name),
        })
    }

    async fn ws_pin(&self, target: &str, pinned: bool) -> Result<(), CliError> {
        let client = self.connect().await?;
        let id = self.resolve_workspace_id(&client, target).await?;
        let ws: WorkspaceInfo = self
            .call(
                &client,
                Request::WorkspacePin(WorkspacePin {
                    workspace_id: id,
                    pinned,
                }),
            )
            .await?;
        self.print(&ws, || {
            let verb = if pinned { "pinned" } else { "unpinned" };
            format!("{verb} {} ({})", ws.id, ws.name)
        })
    }

    async fn ws_recent(&self, limit: u32) -> Result<(), CliError> {
        let client = self.connect().await?;
        let recent: RecentProjects = self
            .call(
                &client,
                Request::WorkspaceRecent(WorkspaceRecent { limit: Some(limit) }),
            )
            .await?;
        self.print(&recent, || recent_table(&recent, now_ms()))
    }

    async fn ws_use(&self, target: &str) -> Result<(), CliError> {
        let client = self.connect().await?;
        let all = self.workspace_list(&client).await?.workspaces;
        let ws = resolve_workspace(&all, target)?;
        let export = format!("export {WORKSPACE_ENV}={}", ws.id);
        self.print(
            &serde_json::json!({"workspace_id": ws.id, "name": ws.name, "export": export}),
            || export.clone(),
        )
    }

    // ---- helpers shared with the session commands ------------------------------------------

    async fn workspace_list(&self, client: &Client) -> Result<WorkspaceList, CliError> {
        self.call(client, Request::WorkspaceList(Empty {})).await
    }

    /// The id of the workspace `target` names (see [`resolve_workspace`]).
    pub(super) async fn resolve_workspace_id(
        &self,
        client: &Client,
        target: &str,
    ) -> Result<WorkspaceId, CliError> {
        let all = self.workspace_list(client).await?.workspaces;
        resolve_workspace(&all, target).map(|w| w.id)
    }
}

/// The workspace `hutan new` and `ls` were told to use: `--ws`, else `$HUTAN_WORKSPACE`.
pub(super) fn workspace_target(flag: Option<String>) -> Option<(String, &'static str)> {
    match flag {
        Some(target) => Some((target, "--ws")),
        None => std::env::var(WORKSPACE_ENV)
            .ok()
            .filter(|value| !value.is_empty())
            .map(|value| (value, WORKSPACE_ENV)),
    }
}

/// Picks one workspace. Tried in order: full id, exact name, name ignoring case, id prefix of at
/// least [`MIN_PREFIX`] characters. A name or prefix matching several workspaces is a usage
/// error that lists the candidates; no match is a plain failure.
pub(super) fn resolve_workspace<'a>(
    workspaces: &'a [WorkspaceInfo],
    target: &str,
) -> Result<&'a WorkspaceInfo, CliError> {
    if let Ok(id) = target.parse::<WorkspaceId>()
        && let Some(workspace) = workspaces.iter().find(|w| w.id == id)
    {
        return Ok(workspace);
    }
    let by_name: Vec<_> = workspaces.iter().filter(|w| w.name == target).collect();
    if !by_name.is_empty() {
        return unique(target, by_name);
    }
    let by_name_folded: Vec<_> = workspaces
        .iter()
        .filter(|w| w.name.to_lowercase() == target.to_lowercase())
        .collect();
    if !by_name_folded.is_empty() {
        return unique(target, by_name_folded);
    }
    if target.len() >= MIN_PREFIX {
        let prefix = target.to_ascii_uppercase();
        let by_prefix: Vec<_> = workspaces
            .iter()
            .filter(|w| w.id.to_string().starts_with(&prefix))
            .collect();
        if !by_prefix.is_empty() {
            return unique(target, by_prefix);
        }
    }
    let hint = if target.len() < MIN_PREFIX {
        format!(" (an id prefix needs at least {MIN_PREFIX} characters)")
    } else {
        String::new()
    };
    Err(CliError::Failed(format!(
        "no workspace matches `{target}`{hint}; `hutan ws ls --all` lists workspaces"
    )))
}

fn unique<'a>(
    target: &str,
    mut matches: Vec<&'a WorkspaceInfo>,
) -> Result<&'a WorkspaceInfo, CliError> {
    match matches.len() {
        1 => Ok(matches.remove(0)),
        n => {
            let mut message = format!("`{target}` matches {n} workspaces; use a longer id:");
            for workspace in matches {
                message.push_str(&format!("\n  {}  {}", workspace.id, workspace.name));
            }
            Err(CliError::Usage(message))
        }
    }
}

/// Without `--all`: Home, pinned workspaces, and those that have sessions.
fn is_listed_by_default(workspace: &WorkspaceInfo) -> bool {
    workspace.kind == WorkspaceKind::Home || workspace.pinned || workspace.total_sessions > 0
}

/// Sessions per workspace that wait for the user: a pending approval, or an agent that reports
/// being blocked on one.
fn attention_counts(sessions: &[SessionInfo]) -> HashMap<WorkspaceId, u32> {
    let mut counts = HashMap::new();
    for session in sessions {
        let waiting = session.state == SessionState::WaitingApproval
            || (session.state.is_live()
                && session
                    .agent
                    .as_ref()
                    .is_some_and(|report| report.state == AgentState::Blocked));
        if waiting {
            *counts.entry(session.workspace_id).or_insert(0) += 1;
        }
    }
    counts
}

/// Pinned workspaces first, then by `mode`; the daemon's manual order breaks every tie.
fn sort_workspaces(
    workspaces: &mut [WorkspaceInfo],
    mode: WsSort,
    attention: &HashMap<WorkspaceId, u32>,
) {
    workspaces.sort_by(|a, b| {
        let waiting = |w: &WorkspaceInfo| attention.get(&w.id).copied().unwrap_or(0);
        b.pinned.cmp(&a.pinned).then_with(|| {
            match mode {
                WsSort::Manual => std::cmp::Ordering::Equal,
                WsSort::Recent => b.last_active_ms.cmp(&a.last_active_ms),
                WsSort::Attention => waiting(b)
                    .cmp(&waiting(a))
                    .then_with(|| b.last_active_ms.cmp(&a.last_active_ms)),
                WsSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            }
            .then_with(|| a.position.cmp(&b.position))
        })
    });
}

fn workspace_table(workspaces: &[WorkspaceInfo], now_ms: u64) -> String {
    if workspaces.is_empty() {
        return "no workspaces".to_owned();
    }
    let rows = workspaces
        .iter()
        .map(|w| {
            vec![
                w.id.to_string(),
                w.name.clone(),
                match w.kind {
                    WorkspaceKind::Home => "home",
                    WorkspaceKind::Project => "project",
                }
                .to_owned(),
                if w.pinned { "pinned" } else { "" }.to_owned(),
                format!("{}/{}", w.live_sessions, w.total_sessions),
                active_text(w.last_active_ms, now_ms),
                w.roots
                    .iter()
                    .map(|r| r.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ]
        })
        .collect();
    table(
        &["ID", "NAME", "KIND", "PIN", "SESSIONS", "ACTIVE", "ROOTS"],
        rows,
    )
}

fn recent_table(recent: &RecentProjects, now_ms: u64) -> String {
    if recent.projects.is_empty() {
        return "no recent projects".to_owned();
    }
    let rows = recent
        .projects
        .iter()
        .map(|p| {
            vec![
                p.name.clone(),
                p.workspace_id
                    .map_or_else(|| "closed".to_owned(), |id| id.to_string()),
                active_text(p.last_opened_ms, now_ms),
                p.path.display().to_string(),
            ]
        })
        .collect();
    table(&["NAME", "WORKSPACE", "OPENED", "PATH"], rows)
}

/// `5s`, `3m`, ... since `at_ms`; `-` when it never happened.
fn active_text(at_ms: u64, now_ms: u64) -> String {
    if at_ms == 0 {
        "-".to_owned()
    } else {
        age_text(now_ms.saturating_sub(at_ms) / 1000)
    }
}

/// Columns padded to their widest cell, two spaces apart; the last column is not padded.
fn table(header: &[&str], rows: Vec<Vec<String>>) -> String {
    let header: Vec<String> = header.iter().map(|h| (*h).to_owned()).collect();
    let mut widths = vec![0; header.len()];
    for row in std::iter::once(&header).chain(&rows) {
        for (width, cell) in widths.iter_mut().zip(row) {
            *width = (*width).max(cell.chars().count());
        }
    }
    let last = header.len() - 1;
    std::iter::once(&header)
        .chain(&rows)
        .map(|row| {
            let mut line = String::new();
            for (index, (cell, width)) in row.iter().zip(&widths).enumerate() {
                if index == last {
                    line.push_str(cell);
                } else {
                    line.push_str(&format!("{cell:<width$}  "));
                }
            }
            line.trim_end().to_owned()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ws(id: &str, name: &str, position: i64) -> WorkspaceInfo {
        WorkspaceInfo {
            id: id.parse().unwrap(),
            name: name.to_owned(),
            roots: Vec::new(),
            kind: WorkspaceKind::Project,
            pinned: false,
            position,
            created_by: CreatedBy::User,
            last_active_ms: 0,
            live_sessions: 0,
            total_sessions: 0,
        }
    }

    const A: &str = "01J0000000000000000000000A";
    const B: &str = "01J0000000000000000000000B";
    const C: &str = "01K0000000000000000000000C";

    fn fixture() -> Vec<WorkspaceInfo> {
        vec![ws(A, "alpha", 0), ws(B, "Beta", 1), ws(C, "alpha-2", 2)]
    }

    #[test]
    fn resolves_full_id_name_folded_name_and_prefix() {
        let all = fixture();
        assert_eq!(resolve_workspace(&all, A).unwrap().name, "alpha");
        assert_eq!(resolve_workspace(&all, "Beta").unwrap().id.to_string(), B);
        assert_eq!(resolve_workspace(&all, "beta").unwrap().id.to_string(), B);
        assert_eq!(
            resolve_workspace(&all, "01k0").unwrap().id.to_string(),
            C,
            "prefixes are case-insensitive"
        );
    }

    #[test]
    fn ambiguous_targets_are_usage_errors_and_unmatched_ones_fail() {
        let all = fixture();
        let error = resolve_workspace(&all, "01J0").unwrap_err();
        assert!(matches!(error, CliError::Usage(_)), "{error:?}");
        assert!(error.to_string().contains(A) && error.to_string().contains(B));
        for target in ["nope", "01J", "alp"] {
            let error = resolve_workspace(&all, target).unwrap_err();
            assert!(matches!(error, CliError::Failed(_)), "{target}: {error:?}");
        }
        let mut twins = fixture();
        twins[2].name = "alpha".to_owned();
        assert!(matches!(
            resolve_workspace(&twins, "alpha"),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn the_default_listing_hides_empty_workspaces_except_pinned_and_home() {
        let mut all = fixture();
        all[1].total_sessions = 1;
        all[2].pinned = true;
        let mut home = ws("01J0000000000000000000000H", "Home", -1);
        home.kind = WorkspaceKind::Home;
        all.push(home);
        let shown: Vec<_> = all
            .iter()
            .filter(|w| is_listed_by_default(w))
            .map(|w| w.name.as_str())
            .collect();
        assert_eq!(shown, ["Beta", "alpha-2", "Home"]);
    }

    #[test]
    fn sorting_puts_pins_first_and_breaks_ties_by_position() {
        let mut all = fixture();
        all[0].last_active_ms = 10;
        all[1].last_active_ms = 30;
        all[2].last_active_ms = 30;
        let attention = HashMap::from([(all[0].id, 2)]);
        let order = |mode, all: &mut Vec<WorkspaceInfo>| {
            sort_workspaces(all, mode, &attention);
            all.iter().map(|w| w.name.clone()).collect::<Vec<_>>()
        };
        assert_eq!(
            order(WsSort::Manual, &mut all),
            ["alpha", "Beta", "alpha-2"]
        );
        assert_eq!(
            order(WsSort::Recent, &mut all),
            ["Beta", "alpha-2", "alpha"]
        );
        assert_eq!(
            order(WsSort::Attention, &mut all),
            ["alpha", "Beta", "alpha-2"]
        );
        assert_eq!(order(WsSort::Name, &mut all), ["alpha", "alpha-2", "Beta"]);
        all.iter_mut().find(|w| w.name == "alpha-2").unwrap().pinned = true;
        assert_eq!(
            order(WsSort::Manual, &mut all),
            ["alpha-2", "alpha", "Beta"]
        );
        assert_eq!(order(WsSort::Name, &mut all), ["alpha-2", "alpha", "Beta"]);
    }

    #[test]
    fn tables_pad_all_but_the_last_column() {
        let text = table(
            &["A", "BB", "C"],
            vec![vec!["x".into(), "yyy".into(), "z".into()]],
        );
        assert_eq!(text, "A  BB   C\nx  yyy  z");
    }
}
