//! Every statement the actor runs. Pure functions over a connection, so the actor thread is the
//! only place that decides when they run.

use std::path::{Path, PathBuf};

use heeler_core::{SessionId, SessionKind, SessionState, WorkspaceId};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::error::{Result, StorageError};
use crate::rows::{CreatedBy, EpochRecord, RecentProject, SessionRow, WorkspaceKind, WorkspaceRow};

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
    SetMeta {
        key: String,
        value: String,
    },
    /// Appends to the epoch history, keeping the newest [`EPOCH_HISTORY_KEPT`].
    RecordEpoch(EpochRecord),
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
        Write::SetMeta { key, value } => set_meta(conn, key, value)?,
        Write::RecordEpoch(record) => record_epoch(conn, *record)?,
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
    let mut stmt = conn
        .prepare(&format!(
            "SELECT {SESSION_COLUMNS} FROM sessions \
             WHERE ?1 IS NULL OR workspace_id = ?1 \
             ORDER BY created_at_ms, id"
        ))
        .map_err(StorageError::sqlite(context))?;
    let rows = stmt
        .query_map([workspace.map(|w| w.to_string())], |row| {
            Ok(session_from(row))
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
