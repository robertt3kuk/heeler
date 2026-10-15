//! Every statement the actor runs. Pure functions over a connection, so the actor thread is the
//! only place that decides when they run.

use std::path::{Path, PathBuf};

use hutan_core::{SessionId, SessionKind, SessionState, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Result, StorageError};
use crate::rows::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus, CreatedBy, EpochRecord,
    ProviderRow, RecentProject, SessionRow, WorkspaceKind, WorkspaceRow, WorktreeRow,
};

pub const EPOCH_HISTORY_KEY: &str = "epoch_history";

/// Recent projects remembered; the oldest are dropped when a new one is added.
pub const RECENT_PROJECTS_KEPT: usize = 200;

/// Epochs remembered in `app_meta`.
pub const EPOCH_HISTORY_KEPT: usize = 32;

/// A mutation. Applied in submission order by the single writer thread.
#[derive(Debug, Clone)]
pub enum Write {
    /// Inserts the workspace; an existing row only has its name and roots refreshed.
    InsertWorkspace(WorkspaceRow),
    RenameWorkspace {
        id: WorkspaceId,
        name: String,
    },
    SetWorkspaceRoots {
        id: WorkspaceId,
        roots: Vec<PathBuf>,
    },
    SetWorkspacePinned {
        id: WorkspaceId,
        pinned: bool,
    },
    SetWorkspacePosition {
        id: WorkspaceId,
        position: i64,
    },
    SetWorkspaceActive {
        id: WorkspaceId,
        last_active_ms: u64,
    },
    /// Adds or refreshes a recent project (a newer `last_opened_ms` wins; the name is replaced),
    /// keeping the newest [`RECENT_PROJECTS_KEPT`].
    UpsertRecentProject(RecentProject),
    DeleteRecentProject(PathBuf),
    /// Also deletes the workspace's sessions.
    DeleteWorkspace(WorkspaceId),
    /// Inserts or replaces every column except `created_at_ms`.
    UpsertSession(Box<SessionRow>),
    DeleteSession(SessionId),
    /// Stores the observation tree of a session (JSON written by the daemon); the session's row
    /// must have been written before.
    UpsertAgentTree {
        session_id: SessionId,
        tree_json: String,
        updated_at_ms: u64,
    },
    DeleteAgentTree(SessionId),
    SetMeta {
        key: String,
        value: String,
    },
    /// Appends to the epoch history, keeping the newest [`EPOCH_HISTORY_KEPT`].
    RecordEpoch(EpochRecord),
    /// Adds one event to a chat journal (an existing `(session, seq)` is left alone).
    AppendChatItem(Box<ChatItemRow>),
    /// Removes one event of a chat journal (a snapshot that a final event supersedes).
    DeleteChatItem {
        session_id: SessionId,
        seq: u64,
    },
    /// Inserts a chat turn or replaces its status, end time and error.
    UpsertChatTurn(ChatTurnRow),
    /// Inserts an approval or replaces how it ended.
    UpsertApproval(Box<ApprovalRow>),
    /// Inserts a provider or replaces everything but its creation time.
    UpsertProvider(Box<ProviderRow>),
    DeleteProvider(String),
    /// Remembers a worktree Hutan created, replacing the row of the same path.
    UpsertWorktree(Box<WorktreeRow>),
    DeleteWorktree(PathBuf),
    /// Crash-injection points for the tests.
    #[cfg(test)]
    Fault(Fault),
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
pub enum Fault {
    /// Unwinds the writer thread.
    Panic,
    /// Kills the whole process, like a power cut.
    Abort,
}

pub fn apply(conn: &Connection, write: &Write) -> Result<()> {
    match write {
        Write::InsertWorkspace(row) => {
            conn.execute(
                "INSERT INTO workspaces (id, name, roots_json, kind, pinned, created_by, \
                     created_at_ms, last_active_ms, position) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9) \
                 ON CONFLICT (id) DO UPDATE SET name = excluded.name, roots_json = excluded.roots_json",
                params![
                    row.id.to_string(),
                    row.name,
                    encode_paths(&row.roots)?,
                    to_text(&row.kind, "workspace kind")?,
                    row.pinned,
                    to_text(&row.created_by, "workspace origin")?,
                    ms(row.created_at_ms),
                    ms(row.last_active_ms),
                    row.position
                ],
            )
            .map_err(StorageError::sqlite("inserting a workspace"))?;
        }
        Write::SetWorkspaceRoots { id, roots } => {
            conn.execute(
                "UPDATE workspaces SET roots_json = ?2 WHERE id = ?1",
                params![id.to_string(), encode_paths(roots)?],
            )
            .map_err(StorageError::sqlite("updating workspace roots"))?;
        }
        Write::SetWorkspacePinned { id, pinned } => {
            conn.execute(
                "UPDATE workspaces SET pinned = ?2 WHERE id = ?1",
                params![id.to_string(), pinned],
            )
            .map_err(StorageError::sqlite("pinning a workspace"))?;
        }
        Write::SetWorkspacePosition { id, position } => {
            conn.execute(
                "UPDATE workspaces SET position = ?2 WHERE id = ?1",
                params![id.to_string(), position],
            )
            .map_err(StorageError::sqlite("moving a workspace"))?;
        }
        Write::SetWorkspaceActive { id, last_active_ms } => {
            conn.execute(
                "UPDATE workspaces SET last_active_ms = ?2 WHERE id = ?1",
                params![id.to_string(), ms(*last_active_ms)],
            )
            .map_err(StorageError::sqlite("recording workspace activity"))?;
        }
        Write::UpsertRecentProject(project) => upsert_recent_project(conn, project)?,
        Write::DeleteRecentProject(path) => {
            conn.execute(
                "DELETE FROM recent_projects WHERE path = ?1",
                [path_str(path)?],
            )
            .map_err(StorageError::sqlite("forgetting a recent project"))?;
        }
        Write::RenameWorkspace { id, name } => {
            conn.execute(
                "UPDATE workspaces SET name = ?2 WHERE id = ?1",
                params![id.to_string(), name],
            )
            .map_err(StorageError::sqlite("renaming a workspace"))?;
        }
        Write::DeleteWorkspace(id) => {
            conn.execute("DELETE FROM workspaces WHERE id = ?1", [id.to_string()])
                .map_err(StorageError::sqlite("deleting a workspace"))?;
        }
        Write::UpsertSession(row) => upsert_session(conn, row)?,
        Write::DeleteSession(id) => {
            conn.execute("DELETE FROM sessions WHERE id = ?1", [id.to_string()])
                .map_err(StorageError::sqlite("deleting a session"))?;
        }
        Write::UpsertAgentTree {
            session_id,
            tree_json,
            updated_at_ms,
        } => {
            conn.execute(
                "INSERT INTO agent_trees (session_id, tree_json, updated_at_ms) \
                 VALUES (?1, ?2, ?3) \
                 ON CONFLICT (session_id) DO UPDATE SET \
                     tree_json = excluded.tree_json, updated_at_ms = excluded.updated_at_ms",
                params![session_id.to_string(), tree_json, ms(*updated_at_ms)],
            )
            .map_err(StorageError::sqlite("writing an agent tree"))?;
        }
        Write::DeleteAgentTree(session_id) => {
            conn.execute(
                "DELETE FROM agent_trees WHERE session_id = ?1",
                [session_id.to_string()],
            )
            .map_err(StorageError::sqlite("deleting an agent tree"))?;
        }
        Write::SetMeta { key, value } => set_meta(conn, key, value)?,
        Write::RecordEpoch(record) => record_epoch(conn, *record)?,
        Write::AppendChatItem(row) => append_chat_item(conn, row)?,
        Write::DeleteChatItem { session_id, seq } => {
            conn.execute(
                "DELETE FROM chat_items WHERE session_id = ?1 AND seq = ?2",
                params![session_id.to_string(), ms(*seq)],
            )
            .map_err(StorageError::sqlite("removing a chat event"))?;
        }
        Write::UpsertChatTurn(row) => upsert_chat_turn(conn, row)?,
        Write::UpsertApproval(row) => upsert_approval(conn, row)?,
        Write::UpsertProvider(row) => {
            conn.execute(
                "INSERT INTO providers (id, kind, name, base_url, default_model, keychain_ref, \
                     created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
                 ON CONFLICT (id) DO UPDATE SET kind = excluded.kind, name = excluded.name, \
                     base_url = excluded.base_url, default_model = excluded.default_model, \
                     keychain_ref = excluded.keychain_ref",
                params![
                    row.id,
                    row.kind,
                    row.name,
                    row.base_url,
                    row.default_model,
                    row.keychain_ref,
                    ms(row.created_at_ms)
                ],
            )
            .map_err(StorageError::sqlite("writing a provider"))?;
        }
        Write::DeleteProvider(id) => {
            conn.execute("DELETE FROM providers WHERE id = ?1", [id])
                .map_err(StorageError::sqlite("deleting a provider"))?;
        }
        Write::UpsertWorktree(row) => {
            conn.execute(
                "INSERT INTO worktrees (path, workspace_id, repo_root, branch, base, created_at_ms) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
                 ON CONFLICT (path) DO UPDATE SET workspace_id = excluded.workspace_id, \
                     repo_root = excluded.repo_root, branch = excluded.branch, \
                     base = excluded.base, created_at_ms = excluded.created_at_ms",
                params![
                    row.path.to_string_lossy(),
                    row.workspace_id.to_string(),
                    row.repo_root.to_string_lossy(),
                    row.branch,
                    row.base,
                    ms(row.created_at_ms)
                ],
            )
            .map_err(StorageError::sqlite("writing a worktree"))?;
        }
        Write::DeleteWorktree(path) => {
            conn.execute(
                "DELETE FROM worktrees WHERE path = ?1",
                [path.to_string_lossy()],
            )
            .map_err(StorageError::sqlite("deleting a worktree"))?;
        }
        #[cfg(test)]
        Write::Fault(Fault::Panic) => panic!("injected writer panic"),
        #[cfg(test)]
        Write::Fault(Fault::Abort) => std::process::abort(),
    }
    Ok(())
}

fn upsert_session(conn: &Connection, row: &SessionRow) -> Result<()> {
    let state = serde_json::to_string(&row.state).map_err(|e| StorageError::Encode {
        what: "session state",
        detail: e.to_string(),
    })?;
    let cwd = row.cwd.as_deref().map(path_str).transpose()?;
    conn.execute(
        "INSERT INTO sessions (id, workspace_id, kind, title, title_locked, spec_json, cwd, \
             state_json, created_at_ms, ended_at_ms, resume_ref_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (id) DO UPDATE SET \
             workspace_id = excluded.workspace_id, kind = excluded.kind, title = excluded.title, \
             title_locked = excluded.title_locked, spec_json = excluded.spec_json, \
             cwd = excluded.cwd, state_json = excluded.state_json, \
             ended_at_ms = excluded.ended_at_ms, resume_ref_json = excluded.resume_ref_json",
        params![
            row.id.to_string(),
            row.workspace_id.to_string(),
            to_text(&row.kind, "session kind")?,
            row.title,
            row.title_locked,
            row.spec_json,
            cwd,
            state,
            ms(row.created_at_ms),
            row.ended_at_ms.map(ms),
            row.resume_ref_json,
        ],
    )
    .map_err(StorageError::sqlite("writing a session"))?;
    Ok(())
}

fn upsert_recent_project(conn: &Connection, project: &RecentProject) -> Result<()> {
    let context = "remembering a recent project";
    conn.execute(
        "INSERT INTO recent_projects (path, name, last_opened_ms) VALUES (?1, ?2, ?3) \
         ON CONFLICT (path) DO UPDATE SET name = excluded.name, \
             last_opened_ms = MAX(last_opened_ms, excluded.last_opened_ms)",
        params![
            path_str(&project.path)?,
            project.name,
            ms(project.last_opened_ms)
        ],
    )
    .map_err(StorageError::sqlite(context))?;
    conn.execute(
        "DELETE FROM recent_projects WHERE path NOT IN \
             (SELECT path FROM recent_projects ORDER BY last_opened_ms DESC, path LIMIT ?1)",
        [i64::try_from(RECENT_PROJECTS_KEPT).unwrap_or(i64::MAX)],
    )
    .map_err(StorageError::sqlite(context))?;
    Ok(())
}

pub fn list_recent_projects(conn: &Connection) -> Result<Vec<RecentProject>> {
    let context = "listing recent projects";
    let mut stmt = conn
        .prepare(
            "SELECT path, name, last_opened_ms FROM recent_projects \
             ORDER BY last_opened_ms DESC, path",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(RecentProject {
                path: PathBuf::from(row.get::<_, String>(0)?),
                name: row.get(1)?,
                last_opened_ms: unms(row.get(2)?),
            })
        })
        .map_err(StorageError::sqlite(context))?;
    rows.map(|row| row.map_err(StorageError::sqlite(context)))
        .collect()
}

pub fn set_meta(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        [key, value],
    )
    .map_err(StorageError::sqlite("writing app_meta"))?;
    Ok(())
}

pub fn get_meta(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM app_meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .optional()
    .map_err(StorageError::sqlite("reading app_meta"))
}

fn record_epoch(conn: &Connection, record: EpochRecord) -> Result<()> {
    let mut history = epoch_history(conn)?;
    history.push(record);
    let excess = history.len().saturating_sub(EPOCH_HISTORY_KEPT);
    history.drain(..excess);
    let json = serde_json::to_string(&history).map_err(|e| StorageError::Encode {
        what: "epoch history",
        detail: e.to_string(),
    })?;
    set_meta(conn, EPOCH_HISTORY_KEY, &json)
}

pub fn epoch_history(conn: &Connection) -> Result<Vec<EpochRecord>> {
    match get_meta(conn, EPOCH_HISTORY_KEY)? {
        None => Ok(Vec::new()),
        Some(json) => serde_json::from_str(&json)
            .map_err(|e| StorageError::corrupt("app_meta.epoch_history", e)),
    }
}

pub fn list_workspaces(conn: &Connection) -> Result<Vec<WorkspaceRow>> {
    let mut stmt = conn
        .prepare(
            "SELECT id, name, roots_json, kind, pinned, created_by, created_at_ms, \
                 last_active_ms, position FROM workspaces \
             ORDER BY position, created_at_ms, id",
        )
        .map_err(StorageError::sqlite("listing workspaces"))?;
    let rows = stmt
        .query_map([], |row| Ok(workspace_from(row)))
        .map_err(StorageError::sqlite("listing workspaces"))?;
    collect(rows, "listing workspaces")
}

const SESSION_COLUMNS: &str = "id, workspace_id, kind, title, title_locked, spec_json, cwd, \
     state_json, created_at_ms, ended_at_ms, resume_ref_json";

pub fn list_sessions(conn: &Connection, workspace: Option<WorkspaceId>) -> Result<Vec<SessionRow>> {
    let context = "listing sessions";
    // Match the registry's insertion order at timestamp ties. ULID randomness is not a
    // creation-order tie breaker; upsert preserves rowid (also used for provider ordering).
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             WHERE ?1 IS NULL OR workspace_id = ?1 \
             ORDER BY created_at_ms, rowid"
        ))
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([workspace.map(|w| w.to_string())], |row| {
            Ok(session_from(row))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

/// The stored agent trees: `(session, tree JSON)`.
pub fn list_agent_trees(conn: &Connection) -> Result<Vec<(SessionId, String)>> {
    let context = "listing agent trees";
    let mut stmt = conn
        .prepare("SELECT session_id, tree_json FROM agent_trees ORDER BY session_id")
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(0)?;
            let json: String = row.get(1)?;
            Ok(id
                .parse::<SessionId>()
                .map(|id| (id, json))
                .map_err(|e| StorageError::corrupt("agent_trees.session_id", e.to_string())))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

fn collect<T>(
    rows: impl Iterator<Item = rusqlite::Result<Result<T>>>,
    context: &str,
) -> Result<Vec<T>> {
    rows.map(|row| row.map_err(StorageError::sqlite(context))?)
        .collect()
}

fn workspace_from(row: &Row<'_>) -> Result<WorkspaceRow> {
    let id: String = row.get(0).map_err(read_err)?;
    let roots_json: String = row.get(2).map_err(read_err)?;
    let kind: String = row.get(3).map_err(read_err)?;
    let created_by: String = row.get(5).map_err(read_err)?;
    let at = |column: &str| format!("workspaces.{column} of {id}");
    Ok(WorkspaceRow {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("workspaces.id", e))?,
        name: row.get(1).map_err(read_err)?,
        roots: decode_paths(&roots_json, &id)?,
        kind: from_text::<WorkspaceKind>(&kind)
            .map_err(|e| StorageError::corrupt(at("kind"), e))?,
        pinned: row.get(4).map_err(read_err)?,
        created_by: from_text::<CreatedBy>(&created_by)
            .map_err(|e| StorageError::corrupt(at("created_by"), e))?,
        created_at_ms: unms(row.get(6).map_err(read_err)?),
        last_active_ms: unms(row.get(7).map_err(read_err)?),
        position: row.get(8).map_err(read_err)?,
    })
}

fn session_from(row: &Row<'_>) -> Result<SessionRow> {
    let id: String = row.get(0).map_err(read_err)?;
    let at = |column: &str| format!("sessions.{column} of {id}");
    let workspace_id: String = row.get(1).map_err(read_err)?;
    let kind: String = row.get(2).map_err(read_err)?;
    let state_json: String = row.get(7).map_err(read_err)?;
    let cwd: Option<String> = row.get(6).map_err(read_err)?;
    Ok(SessionRow {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("sessions.id", e))?,
        workspace_id: workspace_id
            .parse()
            .map_err(|e| StorageError::corrupt(at("workspace_id"), e))?,
        kind: from_text::<SessionKind>(&kind).map_err(|e| StorageError::corrupt(at("kind"), e))?,
        title: row.get(3).map_err(read_err)?,
        title_locked: row.get(4).map_err(read_err)?,
        spec_json: row.get(5).map_err(read_err)?,
        cwd: cwd.map(PathBuf::from),
        state: serde_json::from_str::<SessionState>(&state_json)
            .map_err(|e| StorageError::corrupt(at("state_json"), e))?,
        created_at_ms: unms(row.get(8).map_err(read_err)?),
        ended_at_ms: row.get::<_, Option<i64>>(9).map_err(read_err)?.map(unms),
        resume_ref_json: row.get(10).map_err(read_err)?,
    })
}

fn read_err(source: rusqlite::Error) -> StorageError {
    StorageError::Sqlite {
        context: "reading a row".to_owned(),
        source,
    }
}

fn ms(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn unms(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

fn path_str(path: &Path) -> Result<&str> {
    path.to_str().ok_or_else(|| StorageError::Encode {
        what: "path",
        detail: format!("{} is not valid UTF-8", path.display()),
    })
}

fn encode_paths(paths: &[PathBuf]) -> Result<String> {
    let strings = paths
        .iter()
        .map(|p| path_str(p))
        .collect::<Result<Vec<_>>>()?;
    serde_json::to_string(&strings).map_err(|e| StorageError::Encode {
        what: "workspace roots",
        detail: e.to_string(),
    })
}

fn decode_paths(json: &str, workspace: &str) -> Result<Vec<PathBuf>> {
    let strings: Vec<String> = serde_json::from_str(json)
        .map_err(|e| StorageError::corrupt(format!("workspaces.roots_json of {workspace}"), e))?;
    Ok(strings.into_iter().map(PathBuf::from).collect())
}

/// A unit enum as its bare serde name (`terminal`), the same text the JSON columns use.
fn to_text<T: Serialize>(value: &T, what: &'static str) -> Result<String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        Ok(other) => Err(StorageError::Encode {
            what,
            detail: format!("expected a string, got {other}"),
        }),
        Err(e) => Err(StorageError::Encode {
            what,
            detail: e.to_string(),
        }),
    }
}

fn from_text<T: DeserializeOwned>(text: &str) -> serde_json::Result<T> {
    serde_json::from_value(serde_json::Value::String(text.to_owned()))
}

// Chat journal ---------------------------------------------------------------------------------

fn append_chat_item(conn: &Connection, row: &ChatItemRow) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO chat_items (session_id, seq, ts_ms, turn_id, item_id, kind, event_json) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            row.session_id.to_string(),
            ms(row.seq),
            ms(row.ts_ms),
            row.turn_id,
            row.item_id,
            row.kind,
            row.event_json
        ],
    )
    .map_err(StorageError::sqlite("journaling a chat event"))?;
    Ok(())
}

/// Up to `limit` events with `seq < before` (all when `None`), newest window first picked, then
/// returned oldest first.
pub fn chat_items(
    conn: &Connection,
    session: SessionId,
    before: Option<u64>,
    limit: u32,
) -> Result<Vec<ChatItemRow>> {
    let context = "reading a chat journal";
    let mut stmt = conn
        .prepare(
            "SELECT seq, ts_ms, turn_id, item_id, kind, event_json FROM chat_items \
             WHERE session_id = ?1 AND seq < ?2 ORDER BY seq DESC LIMIT ?3",
        )
        .map_err(StorageError::sqlite(context))?;
    let before = before.map_or(i64::MAX, ms);
    let rows = stmt
        .query_map(
            params![session.to_string(), before, i64::from(limit)],
            |row| {
                Ok(ChatItemRow {
                    session_id: session,
                    seq: unms(row.get(0)?),
                    ts_ms: unms(row.get(1)?),
                    turn_id: row.get(2)?,
                    item_id: row.get(3)?,
                    kind: row.get(4)?,
                    event_json: row.get(5)?,
                })
            },
        )
        .map_err(StorageError::sqlite(context))?;
    let mut items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))?;
    items.reverse();
    Ok(items)
}

/// The newest `limit` events of one `kind` of the session, oldest first.
pub fn chat_items_of_kind(
    conn: &Connection,
    session: SessionId,
    kind: &str,
    limit: u32,
) -> Result<Vec<ChatItemRow>> {
    let context = "reading chat events of one kind";
    let mut stmt = conn
        .prepare(
            "SELECT seq, ts_ms, turn_id, item_id, kind, event_json FROM chat_items \
             WHERE session_id = ?1 AND kind = ?2 ORDER BY seq DESC LIMIT ?3",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(
            params![session.to_string(), kind, i64::from(limit)],
            |row| {
                Ok(ChatItemRow {
                    session_id: session,
                    seq: unms(row.get(0)?),
                    ts_ms: unms(row.get(1)?),
                    turn_id: row.get(2)?,
                    item_id: row.get(3)?,
                    kind: row.get(4)?,
                    event_json: row.get(5)?,
                })
            },
        )
        .map_err(StorageError::sqlite(context))?;
    let mut items = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))?;
    items.reverse();
    Ok(items)
}

/// The highest journaled `seq` of the session (0 for an empty journal).
pub fn chat_last_seq(conn: &Connection, session: SessionId) -> Result<u64> {
    let last: Option<i64> = conn
        .query_row(
            "SELECT MAX(seq) FROM chat_items WHERE session_id = ?1",
            [session.to_string()],
            |row| row.get(0),
        )
        .map_err(StorageError::sqlite("reading the last chat seq"))?;
    Ok(last.map_or(0, unms))
}

fn upsert_chat_turn(conn: &Connection, row: &ChatTurnRow) -> Result<()> {
    conn.execute(
        "INSERT INTO chat_turns (session_id, turn_id, provider_turn_id, status, started_at_ms, \
             ended_at_ms, error) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
         ON CONFLICT (session_id, turn_id) DO UPDATE SET \
             provider_turn_id = COALESCE(excluded.provider_turn_id, provider_turn_id), \
             status = excluded.status, ended_at_ms = excluded.ended_at_ms, error = excluded.error",
        params![
            row.session_id.to_string(),
            row.turn_id,
            row.provider_turn_id,
            to_text(&row.status, "turn status")?,
            ms(row.started_at_ms),
            row.ended_at_ms.map(ms),
            row.error
        ],
    )
    .map_err(StorageError::sqlite("writing a chat turn"))?;
    Ok(())
}

pub fn chat_turns(conn: &Connection, session: SessionId) -> Result<Vec<ChatTurnRow>> {
    let context = "reading chat turns";
    let mut stmt = conn
        .prepare(
            "SELECT turn_id, provider_turn_id, status, started_at_ms, ended_at_ms, error \
             FROM chat_turns WHERE session_id = ?1 ORDER BY started_at_ms, turn_id",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([session.to_string()], |row| {
            let status: String = row.get(2)?;
            Ok((
                ChatTurnRow {
                    session_id: session,
                    turn_id: row.get(0)?,
                    provider_turn_id: row.get(1)?,
                    status: ChatTurnStatus::Running,
                    started_at_ms: unms(row.get(3)?),
                    ended_at_ms: row.get::<_, Option<i64>>(4)?.map(unms),
                    error: row.get(5)?,
                },
                status,
            ))
        })
        .map_err(StorageError::sqlite(context))?;
    let mut turns = Vec::new();
    for row in rows {
        let (mut turn, status) = row.map_err(StorageError::sqlite(context))?;
        turn.status = from_text::<ChatTurnStatus>(&status).map_err(|e| {
            StorageError::corrupt(format!("chat_turns.status of {}", turn.turn_id), e)
        })?;
        turns.push(turn);
    }
    Ok(turns)
}

fn upsert_approval(conn: &Connection, row: &ApprovalRow) -> Result<()> {
    let outcome = row
        .outcome
        .as_ref()
        .map(|o| to_text(o, "approval outcome"))
        .transpose()?;
    conn.execute(
        "INSERT INTO approvals (id, session_id, turn_id, kind, title, options_json, \
             requested_at_ms, resolved_at_ms, outcome, option_id, decided_by) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11) \
         ON CONFLICT (id) DO UPDATE SET resolved_at_ms = excluded.resolved_at_ms, \
             outcome = excluded.outcome, option_id = excluded.option_id, \
             decided_by = excluded.decided_by",
        params![
            row.id.to_string(),
            row.session_id.to_string(),
            row.turn_id,
            row.kind,
            row.title,
            row.options_json,
            ms(row.requested_at_ms),
            row.resolved_at_ms.map(ms),
            outcome,
            row.option_id,
            row.decided_by
        ],
    )
    .map_err(StorageError::sqlite("writing an approval"))?;
    Ok(())
}

/// Approvals of one session (or of all, when `None`), oldest first; `only_open` keeps those
/// not resolved yet.
pub fn approvals(
    conn: &Connection,
    session: Option<SessionId>,
    only_open: bool,
) -> Result<Vec<ApprovalRow>> {
    let context = "reading approvals";
    let mut stmt = conn
        .prepare(
            "SELECT id, session_id, turn_id, kind, title, options_json, requested_at_ms, \
                 resolved_at_ms, outcome, option_id, decided_by FROM approvals \
             WHERE (?1 IS NULL OR session_id = ?1) AND (?2 = 0 OR resolved_at_ms IS NULL) \
             ORDER BY requested_at_ms, id",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map(params![session.map(|s| s.to_string()), only_open], |row| {
            Ok(approval_from(row))
        })
        .map_err(StorageError::sqlite(context))?;
    collect(rows, context)
}

fn approval_from(row: &Row<'_>) -> Result<ApprovalRow> {
    let id: String = row.get(0).map_err(read_err)?;
    let session: String = row.get(1).map_err(read_err)?;
    let at = |column: &str| format!("approvals.{column} of {id}");
    let outcome: Option<String> = row.get(8).map_err(read_err)?;
    Ok(ApprovalRow {
        id: id
            .parse()
            .map_err(|e| StorageError::corrupt("approvals.id", e))?,
        session_id: session
            .parse()
            .map_err(|e| StorageError::corrupt(at("session_id"), e))?,
        turn_id: row.get(2).map_err(read_err)?,
        kind: row.get(3).map_err(read_err)?,
        title: row.get(4).map_err(read_err)?,
        options_json: row.get(5).map_err(read_err)?,
        requested_at_ms: unms(row.get(6).map_err(read_err)?),
        resolved_at_ms: row.get::<_, Option<i64>>(7).map_err(read_err)?.map(unms),
        outcome: outcome
            .map(|text| {
                from_text::<ApprovalOutcome>(&text)
                    .map_err(|e| StorageError::corrupt(at("outcome"), e))
            })
            .transpose()?,
        option_id: row.get(9).map_err(read_err)?,
        decided_by: row.get(10).map_err(read_err)?,
    })
}

/// Every provider, oldest first.
pub fn list_providers(conn: &Connection) -> Result<Vec<ProviderRow>> {
    let context = "listing providers";
    let mut stmt = conn
        .prepare(
            "SELECT id, kind, name, base_url, default_model, keychain_ref, created_at_ms \
             FROM providers ORDER BY created_at_ms, rowid",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            Ok(ProviderRow {
                id: row.get(0)?,
                kind: row.get(1)?,
                name: row.get(2)?,
                base_url: row.get(3)?,
                default_model: row.get(4)?,
                keychain_ref: row.get(5)?,
                created_at_ms: unms(row.get(6)?),
            })
        })
        .map_err(StorageError::sqlite(context))?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .map_err(StorageError::sqlite(context))
}

/// Every remembered worktree, oldest first.
pub fn list_worktrees(conn: &Connection) -> Result<Vec<WorktreeRow>> {
    let context = "listing worktrees";
    let mut stmt = conn
        .prepare(
            "SELECT path, workspace_id, repo_root, branch, base, created_at_ms \
             FROM worktrees ORDER BY created_at_ms, rowid",
        )
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([], |row| {
            let id: String = row.get(1)?;
            let path: String = row.get(0)?;
            let repo_root: String = row.get(2)?;
            Ok((
                path,
                id,
                repo_root,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(StorageError::sqlite(context))?;
    let mut out = Vec::new();
    for entry in rows {
        let (path, id, repo_root, branch, base, created) =
            entry.map_err(StorageError::sqlite(context))?;
        let workspace_id = id
            .parse()
            .map_err(|e| StorageError::corrupt(format!("worktrees.workspace_id of {path}"), e))?;
        out.push(WorktreeRow {
            path: PathBuf::from(path),
            workspace_id,
            repo_root: PathBuf::from(repo_root),
            branch,
            base,
            created_at_ms: unms(created),
        });
    }
    Ok(out)
}
