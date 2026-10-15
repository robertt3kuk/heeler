#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use heeler_core::{Epoch, SessionId, SessionKind, SessionState, WorkspaceId};
use tempfile::TempDir;

use crate::migrate::Migration;
use crate::sql::Fault;
use crate::{
    ApprovalOutcome, ApprovalRow, ChatItemRow, ChatTurnRow, ChatTurnStatus, CreatedBy, ProviderRow,
    RecentProject, SessionRow, Storage, StorageError, WorkspaceKind, WorkspaceRow, Write, migrate,
};
use heeler_core::ApprovalId;

fn db(dir: &TempDir) -> PathBuf {
    dir.path().join("heeler.db")
}

fn workspace(name: &str, position: i64) -> WorkspaceRow {
    WorkspaceRow {
        id: WorkspaceId::new(),
        name: name.to_owned(),
        roots: vec![PathBuf::from("/work/a"), PathBuf::from("/work/b")],
        kind: WorkspaceKind::Project,
        pinned: false,
        created_by: CreatedBy::User,
        created_at_ms: 1_000 + u64::try_from(position).unwrap(),
        last_active_ms: 2_000 + u64::try_from(position).unwrap(),
        position,
    }
}

fn session(workspace: WorkspaceId, title: &str) -> SessionRow {
    SessionRow {
        id: SessionId::new(),
        workspace_id: workspace,
        kind: SessionKind::Terminal,
        title: title.to_owned(),
        title_locked: false,
        spec_json: r#"{"command":["zsh"]}"#.to_owned(),
        cwd: Some(PathBuf::from("/tmp")),
        state: SessionState::Running,
        created_at_ms: 5_000,
        ended_at_ms: None,
        resume_ref_json: None,
    }
}

const V1: Migration = migrate::MIGRATIONS[0];
const V2: Migration = migrate::MIGRATIONS[1];
const V3_OK: Migration = Migration {
    version: 3,
    name: "extra",
    sql: "CREATE TABLE extra (x INTEGER NOT NULL);",
};
const V3_BROKEN: Migration = Migration {
    version: 3,
    name: "broken",
    // The first statement succeeds; the second must roll it back.
    sql: "CREATE TABLE extra (x INTEGER NOT NULL); INSERT INTO no_such_table VALUES (1);",
};

fn table_exists(path: &Path, table: &str) -> bool {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1)",
        [table],
        |row| row.get(0),
    )
    .unwrap()
}

fn integrity_ok(path: &Path) {
    let conn = rusqlite::Connection::open(path).unwrap();
    let verdict: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(verdict, "ok");
}

fn backups(dir: &TempDir) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir.path().join("backups")) else {
        return Vec::new();
    };
    let mut files: Vec<_> = entries.map(|e| e.unwrap().path()).collect();
    files.sort();
    files
}

#[test]
fn opens_an_empty_database_and_migrates_it() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();

    assert_eq!(storage.app_meta().schema_version().unwrap(), 5);
    assert!(storage.workspaces().list().unwrap().is_empty());
    assert!(storage.sessions().list().unwrap().is_empty());
    assert!(backups(&dir).is_empty(), "a fresh database needs no backup");
    storage.close();

    for table in ["app_meta", "workspaces", "sessions", "recent_projects"] {
        assert!(table_exists(&db(&dir), table), "{table}");
    }
    let conn = rusqlite::Connection::open(db(&dir)).unwrap();
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    use std::os::unix::fs::PermissionsExt;
    let file_mode = fs::metadata(db(&dir)).unwrap().permissions().mode() & 0o777;
    assert_eq!(file_mode, 0o600);
}

#[test]
fn pragmas_apply_to_the_writer_connection() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let (foreign_keys, synchronous): (i64, i64) = storage
        .read(|conn| {
            Ok((
                conn.query_row("PRAGMA foreign_keys", [], |r| r.get(0))
                    .unwrap(),
                conn.query_row("PRAGMA synchronous", [], |r| r.get(0))
                    .unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(foreign_keys, 1);
    assert_eq!(synchronous, 1, "NORMAL");
}

#[test]
fn reopening_is_idempotent_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    let sess = session(ws.id, "shell");
    {
        let storage = Storage::open(db(&dir)).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.sessions().upsert(sess.clone()).unwrap();
        storage.close();
    }
    for _ in 0..2 {
        let storage = Storage::open(db(&dir)).unwrap();
        assert_eq!(storage.app_meta().schema_version().unwrap(), 5);
        assert_eq!(
            storage.workspaces().list().unwrap(),
            std::slice::from_ref(&ws)
        );
        assert_eq!(
            storage.sessions().list().unwrap(),
            std::slice::from_ref(&sess)
        );
        storage.close();
    }
    assert!(
        backups(&dir).is_empty(),
        "nothing was pending, nothing to back up"
    );
}

#[test]
fn a_backup_is_made_before_a_pending_migration() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.close();
    }
    let storage = Storage::open_with_migrations(db(&dir), &[V1, V2, V3_OK]).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), 3);
    storage.close();
    assert!(table_exists(&db(&dir), "extra"));

    let files = backups(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("heeler-") && name.ends_with("-v2.db"),
        "{name}"
    );
    // The backup is the pre-migration database: version 1, data present, no `extra` table.
    assert!(!table_exists(&files[0], "extra"));
    let old = Storage::open_with_migrations(&files[0], &[V1, V2]).unwrap();
    assert_eq!(old.workspaces().list().unwrap(), [ws]);
    old.close();
}

#[test]
fn a_failed_migration_keeps_the_original_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.close();
    }

    let error = Storage::open_with_migrations(db(&dir), &[V1, V2, V3_BROKEN]).unwrap_err();
    let StorageError::Migration {
        version,
        name,
        backup,
        ..
    } = &error
    else {
        panic!("expected a migration error, got {error}");
    };
    assert_eq!((*version, name.as_str()), (3, "broken"));
    assert!(backup.as_ref().is_some_and(|b| b.exists()));
    let text = error.to_string();
    assert!(text.contains("migration 3 (broken) failed"), "{text}");
    assert!(text.contains("left unchanged"), "{text}");

    // Nothing of the failed migration is visible; the data and version are as before.
    assert!(!table_exists(&db(&dir), "extra"));
    let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), 2);
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    storage.close();
    integrity_ok(&db(&dir));
}

#[test]
fn a_database_from_a_newer_build_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    Storage::open_with_migrations(db(&dir), &[V1, V2, V3_OK])
        .unwrap()
        .close();
    let error = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap_err();
    assert!(
        matches!(
            error,
            StorageError::SchemaTooNew {
                found: 3,
                supported: 2,
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn only_the_newest_five_backups_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    Storage::open_with_migrations(db(&dir), &[V1, V2])
        .unwrap()
        .close();
    let backup_dir = dir.path().join("backups");
    fs::create_dir_all(&backup_dir).unwrap();
    for stamp in 1..=7_u64 {
        fs::write(backup_dir.join(format!("heeler-{stamp:013}-v1.db")), b"old").unwrap();
    }
    fs::write(backup_dir.join("notes.txt"), b"not a backup").unwrap();

    Storage::open_with_migrations(db(&dir), &[V1, V2, V3_OK])
        .unwrap()
        .close();

    let names: Vec<String> = backups(&dir)
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        names.iter().filter(|n| n.starts_with("heeler-")).count(),
        5,
        "{names:?}"
    );
    assert!(
        names.contains(&"notes.txt".to_owned()),
        "foreign files stay: {names:?}"
    );
    // The oldest fakes went; the new backup (current time) is the newest.
    assert!(!names.contains(&"heeler-0000000000001-v1.db".to_owned()));
    assert!(names.iter().any(|n| n.as_str() > "heeler-0001000000000"));
}

#[test]
fn workspaces_and_sessions_roundtrip_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let second = workspace("second", 2);
    let first = workspace("first", 1);
    storage.workspaces().insert(second.clone()).unwrap();
    storage.workspaces().insert(first.clone()).unwrap();
    assert_eq!(
        storage.workspaces().list().unwrap(),
        [first.clone(), second.clone()]
    );

    storage
        .workspaces()
        .rename(first.id, "renamed".to_owned())
        .unwrap();
    let renamed = storage.workspaces().list().unwrap();
    assert_eq!(renamed[0].name, "renamed");
    assert_eq!(renamed[0].roots, first.roots);

    let mut a = session(first.id, "a");
    a.created_at_ms = 10;
    let mut b = session(second.id, "b");
    b.created_at_ms = 20;
    b.state = SessionState::Exited {
        code: Some(3),
        signal: None,
    };
    b.ended_at_ms = Some(30);
    b.title_locked = true;
    b.cwd = None;
    b.resume_ref_json = Some(r#"{"argv":["codex","resume","x"]}"#.to_owned());
    storage.sessions().upsert(b.clone()).unwrap();
    storage.sessions().upsert(a.clone()).unwrap();
    assert_eq!(storage.sessions().list().unwrap(), [a.clone(), b.clone()]);
    assert_eq!(
        storage.sessions().list_in_workspace(second.id).unwrap(),
        [b.clone()]
    );

    // An upsert replaces the mutable columns but never the creation time.
    let mut changed = a.clone();
    changed.title = "a2".to_owned();
    changed.state = SessionState::Stopped;
    changed.created_at_ms = 999;
    storage.sessions().upsert(changed).unwrap();
    let stored = &storage.sessions().list_in_workspace(first.id).unwrap()[0];
    assert_eq!(stored.title, "a2");
    assert_eq!(stored.state, SessionState::Stopped);
    assert_eq!(stored.created_at_ms, 10);

    // Deleting a workspace takes its sessions with it.
    storage.workspaces().delete(second.id).unwrap();
    assert_eq!(storage.workspaces().list().unwrap().len(), 1);
    let left = storage.sessions().list().unwrap();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, a.id);

    storage.sessions().delete(a.id).unwrap();
    assert!(storage.sessions().list().unwrap().is_empty());
}

#[test]
fn equal_timestamp_sessions_keep_creation_order_after_updates_and_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let ws = workspace("project", 0);
    storage.workspaces().insert(ws.clone()).unwrap();
    let mut first = session(ws.id, "first assistant");
    let mut second = session(ws.id, "second assistant");
    // ULID randomness within a millisecond may put a later session before the first one.
    first.id = "00000000000000000000000002".parse().unwrap();
    second.id = "00000000000000000000000001".parse().unwrap();
    storage.sessions().upsert(first.clone()).unwrap();
    storage.sessions().upsert(second.clone()).unwrap();
    assert_eq!(
        storage.sessions().list_in_workspace(ws.id).unwrap(),
        [first.clone(), second.clone()]
    );
    first.title = "renamed first assistant".into();
    storage.sessions().upsert(first.clone()).unwrap();
    storage.close();
    let reopened = Storage::open(db(&dir)).unwrap();
    assert_eq!(reopened.sessions().list().unwrap(), [first, second]);
    reopened.close();
}

/// A database as schema v1 wrote it: raw SQL, because the typed API speaks the newest schema.
fn seed_v1(path: &Path, default: WorkspaceId, other: WorkspaceId) {
    let conn = rusqlite::Connection::open(path).unwrap();
    conn.execute_batch(V1.sql).unwrap();
    conn.execute(
        "INSERT INTO app_meta (key, value) VALUES ('schema_version', '1'), ('default_workspace_id', ?1)",
        [default.to_string()],
    )
    .unwrap();
    for (id, name, roots, created, position) in [
        (default, "default", "[]", 100, 0),
        (other, "api", r#"["/work/api"]"#, 200, 1),
    ] {
        conn.execute(
            "INSERT INTO workspaces (id, name, roots_json, created_at_ms, position) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id.to_string(), name, roots, created, position],
        )
        .unwrap();
    }
    for (workspace, created, ended) in [(default, 500_i64, None), (other, 300, Some(900_i64))] {
        conn.execute(
            "INSERT INTO sessions (id, workspace_id, kind, title, title_locked, spec_json, \
                 state_json, created_at_ms, ended_at_ms) \
             VALUES (?1, ?2, 'terminal', 't', 0, '{}', '{\"state\":\"running\"}', ?3, ?4)",
            rusqlite::params![
                SessionId::new().to_string(),
                workspace.to_string(),
                created,
                ended
            ],
        )
        .unwrap();
    }
}

#[test]
fn migrating_v1_turns_the_default_workspace_into_home() {
    let dir = tempfile::tempdir().unwrap();
    let (default, api) = (WorkspaceId::new(), WorkspaceId::new());
    seed_v1(&db(&dir), default, api);

    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), 5);
    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows.len(), 2);
    let home = &rows[0];
    assert_eq!(home.id, default);
    assert_eq!(home.kind, WorkspaceKind::Home);
    assert_eq!(home.name, "Home", "the placeholder name follows");
    assert!(!home.pinned);
    assert_eq!(home.created_by, CreatedBy::User);
    assert_eq!(home.position, 0);
    // Activity: the newest of creation and end of its sessions.
    assert_eq!(home.last_active_ms, 500);
    let project = &rows[1];
    assert_eq!(project.id, api);
    assert_eq!(project.kind, WorkspaceKind::Project);
    assert_eq!(project.name, "api");
    assert_eq!(project.roots, [PathBuf::from("/work/api")]);
    assert_eq!(project.last_active_ms, 900);
    // The pointer is gone: Home is the row with kind `home`.
    assert_eq!(
        storage.app_meta().get("default_workspace_id").unwrap(),
        None
    );
    assert!(storage.recent_projects().list().unwrap().is_empty());
    assert_eq!(
        storage.sessions().list().unwrap().len(),
        2,
        "sessions survive"
    );
    storage.close();

    let files = backups(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    assert!(files[0].to_string_lossy().ends_with("-v1.db"));
}

#[test]
fn migrating_keeps_a_renamed_default_workspace_name() {
    let dir = tempfile::tempdir().unwrap();
    let (default, api) = (WorkspaceId::new(), WorkspaceId::new());
    seed_v1(&db(&dir), default, api);
    {
        let conn = rusqlite::Connection::open(db(&dir)).unwrap();
        conn.execute(
            "UPDATE workspaces SET name = 'notes' WHERE id = ?1",
            [default.to_string()],
        )
        .unwrap();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    let rows = storage.workspaces().list().unwrap();
    assert_eq!(
        (rows[0].name.as_str(), rows[0].kind),
        ("notes", WorkspaceKind::Home)
    );
    storage.close();
}

#[test]
fn there_is_at_most_one_home() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let home = |name: &str, position| WorkspaceRow {
        kind: WorkspaceKind::Home,
        ..workspace(name, position)
    };
    let (first, second) = (home("Home", 0), home("Other", 1));
    storage.workspaces().insert(first.clone()).unwrap();
    // Rejected by the unique index; the failed write is logged and nothing is stored.
    storage.workspaces().insert(second).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [first]);
}

#[test]
fn workspace_fields_are_updated_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let a = WorkspaceRow {
        created_by: CreatedBy::Agent,
        ..workspace("a", 0)
    };
    let b = workspace("b", 1);
    storage.workspaces().insert(a.clone()).unwrap();
    storage.workspaces().insert(b.clone()).unwrap();

    storage.workspaces().set_pinned(b.id, true).unwrap();
    storage
        .workspaces()
        .set_roots(a.id, vec![PathBuf::from("/x")])
        .unwrap();
    storage.workspaces().set_last_active(a.id, 77_000).unwrap();
    storage
        .batch(vec![
            Write::SetWorkspacePosition {
                id: a.id,
                position: 5,
            },
            Write::SetWorkspacePosition {
                id: b.id,
                position: 3,
            },
        ])
        .unwrap();

    let rows = storage.workspaces().list().unwrap();
    assert_eq!(rows.iter().map(|w| w.id).collect::<Vec<_>>(), [b.id, a.id]);
    assert!(rows[0].pinned && !rows[1].pinned);
    assert_eq!(rows[0].position, 3);
    assert_eq!(rows[1].roots, [PathBuf::from("/x")]);
    assert_eq!(rows[1].last_active_ms, 77_000);
    assert_eq!(rows[1].created_by, CreatedBy::Agent);
    assert_eq!(rows[1].created_at_ms, a.created_at_ms);
}

#[test]
fn recent_projects_keep_the_newest_time_and_the_newest_two_hundred() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let project = |path: &str, name: &str, at| RecentProject {
        path: PathBuf::from(path),
        name: name.to_owned(),
        last_opened_ms: at,
    };
    let recent = storage.recent_projects();
    recent.upsert(project("/p/a", "a", 10)).unwrap();
    recent.upsert(project("/p/b", "b", 30)).unwrap();
    recent.upsert(project("/p/c", "c", 20)).unwrap();
    // A refresh replaces the name but an older time never wins.
    recent.upsert(project("/p/b", "b2", 5)).unwrap();
    recent.upsert(project("/p/a", "a", 40)).unwrap();
    assert_eq!(
        recent.list().unwrap(),
        [
            project("/p/a", "a", 40),
            project("/p/b", "b2", 30),
            project("/p/c", "c", 20)
        ]
    );
    recent.delete(PathBuf::from("/p/c")).unwrap();
    assert_eq!(recent.list().unwrap().len(), 2);

    for n in 0..250_u64 {
        recent
            .upsert(project(&format!("/bulk/{n:03}"), "bulk", 1_000 + n))
            .unwrap();
    }
    let all = recent.list().unwrap();
    assert_eq!(all.len(), crate::RECENT_PROJECTS_KEPT);
    assert_eq!(all[0].path, PathBuf::from("/bulk/249"));
    assert_eq!(all.last().unwrap().path, PathBuf::from("/bulk/050"));
    storage.close();

    // Durable.
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(
        storage.recent_projects().list().unwrap().len(),
        crate::RECENT_PROJECTS_KEPT
    );
}

#[test]
fn a_session_needs_its_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    // Rejected by the foreign key; the failed write is logged and nothing is stored.
    storage
        .sessions()
        .upsert(session(WorkspaceId::new(), "orphan"))
        .unwrap();
    assert!(storage.sessions().list().unwrap().is_empty());
}

#[test]
fn app_meta_and_epoch_history() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let meta = storage.app_meta();
    assert_eq!(meta.get("k").unwrap(), None);
    meta.set("k", "v1").unwrap();
    meta.set("k", "v2").unwrap();
    assert_eq!(meta.get("k").unwrap().as_deref(), Some("v2"));

    let epochs: Vec<Epoch> = (0..crate::EPOCH_HISTORY_KEPT + 3)
        .map(|_| Epoch::new())
        .collect();
    for (i, epoch) in epochs.iter().enumerate() {
        meta.record_epoch(*epoch, i as u64).unwrap();
    }
    let history = meta.epoch_history().unwrap();
    assert_eq!(history.len(), crate::EPOCH_HISTORY_KEPT);
    assert_eq!(history[0].epoch, epochs[3]);
    assert_eq!(history.last().unwrap().epoch, *epochs.last().unwrap());
}

#[test]
fn concurrent_submitters_keep_per_entity_order() {
    const THREADS: usize = 8;
    const ROUNDS: usize = 200;

    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let ws = workspace("w", 0);
    storage.workspaces().insert(ws.clone()).unwrap();

    let rows: Vec<SessionRow> = (0..THREADS)
        .map(|i| session(ws.id, &format!("s{i}")))
        .collect();
    std::thread::scope(|scope| {
        for row in &rows {
            let storage = storage.clone();
            scope.spawn(move || {
                let mut row = row.clone();
                for round in 0..ROUNDS {
                    row.title = format!("round-{round}");
                    storage.sessions().upsert(row.clone()).unwrap();
                    if round % 50 == 0 {
                        // A read observes every write this thread queued before it.
                        let seen = storage
                            .sessions()
                            .list_in_workspace(row.workspace_id)
                            .unwrap();
                        let mine = seen.iter().find(|s| s.id == row.id).unwrap();
                        assert_eq!(mine.title, row.title);
                    }
                }
            });
        }
    });
    storage.flush().unwrap();

    let stored = storage.sessions().list().unwrap();
    assert_eq!(stored.len(), THREADS);
    for row in &rows {
        let mine = stored.iter().find(|s| s.id == row.id).unwrap();
        assert_eq!(mine.title, format!("round-{}", ROUNDS - 1));
    }
}

#[test]
fn writes_after_close_fail_cleanly_and_close_drains_the_queue() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let ws = workspace("w", 0);
    storage.workspaces().insert(ws.clone()).unwrap();
    for i in 0..100 {
        storage
            .sessions()
            .upsert(session(ws.id, &format!("s{i}")))
            .unwrap();
    }
    storage.close();
    storage.close();
    assert!(matches!(
        storage.workspaces().insert(workspace("late", 1)),
        Err(StorageError::Closed)
    ));
    assert!(matches!(
        storage.sessions().list(),
        Err(StorageError::Closed)
    ));

    let reopened = Storage::open(db(&dir)).unwrap();
    assert_eq!(reopened.sessions().list().unwrap().len(), 100);
}

fn batch_around(fault: Fault, ws: WorkspaceId) -> Vec<Write> {
    let mut writes: Vec<Write> = (0..50)
        .map(|i| Write::UpsertSession(Box::new(session(ws, &format!("batch-{i}")))))
        .collect();
    writes.push(Write::Fault(fault));
    writes.push(Write::UpsertSession(Box::new(session(ws, "after-fault"))));
    writes
}

#[test]
fn a_writer_that_dies_mid_batch_leaves_the_database_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("w", 0);
    let committed = session(ws.id, "committed");
    let storage = Storage::open(db(&dir)).unwrap();
    storage.workspaces().insert(ws.clone()).unwrap();
    storage.sessions().upsert(committed.clone()).unwrap();
    storage.flush().unwrap();

    storage.batch(batch_around(Fault::Panic, ws.id)).unwrap();
    // The writer thread unwound; the handle reports it instead of hanging.
    assert!(matches!(storage.flush(), Err(StorageError::Closed)));
    assert!(matches!(
        storage.sessions().upsert(session(ws.id, "late")),
        Err(StorageError::Closed)
    ));
    drop(storage);

    integrity_ok(&db(&dir));
    let reopened = Storage::open(db(&dir)).unwrap();
    assert_eq!(reopened.sessions().list().unwrap(), [committed]);
}

#[test]
fn killing_the_process_mid_batch_leaves_the_database_consistent() {
    let dir = tempfile::tempdir().unwrap();
    let path = db(&dir);
    let ws_id = WorkspaceId::new();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["tests::crash_child", "--exact", "--nocapture"])
        .env("HEELER_STORAGE_CRASH_DB", &path)
        .env("HEELER_STORAGE_CRASH_WS", ws_id.to_string())
        .output()
        .unwrap();
    assert_eq!(
        output.status.signal(),
        Some(6),
        "the child must die of SIGABRT mid-batch: {:?}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    integrity_ok(&path);
    let storage = Storage::open(&path).unwrap();
    let titles: Vec<String> = storage
        .sessions()
        .list()
        .unwrap()
        .into_iter()
        .map(|s| s.title)
        .collect();
    assert_eq!(
        titles,
        ["committed"],
        "the half-applied batch must not be visible"
    );
    assert_eq!(storage.workspaces().list().unwrap().len(), 1);
}

/// Body of the child process of the test above; does nothing in a normal test run.
#[test]
fn crash_child() {
    let (Some(path), Some(ws)) = (
        std::env::var_os("HEELER_STORAGE_CRASH_DB"),
        std::env::var("HEELER_STORAGE_CRASH_WS").ok(),
    ) else {
        return;
    };
    let ws: WorkspaceId = ws.parse().unwrap();
    let storage = Storage::open(path).unwrap();
    storage
        .workspaces()
        .insert(WorkspaceRow {
            id: ws,
            ..workspace("w", 0)
        })
        .unwrap();
    storage.sessions().upsert(session(ws, "committed")).unwrap();
    storage.flush().unwrap();
    storage.batch(batch_around(Fault::Abort, ws)).unwrap();
    // The writer aborts the process while the batch transaction is open.
    std::thread::sleep(std::time::Duration::from_secs(30));
}

// Chat journal -----------------------------------------------------------------------------------

fn chat_item(session: SessionId, seq: u64, kind: &str) -> ChatItemRow {
    ChatItemRow {
        session_id: session,
        seq,
        ts_ms: 10_000 + seq,
        turn_id: Some("turn-1".to_owned()),
        item_id: None,
        kind: kind.to_owned(),
        event_json: format!(r#"{{"type":"{kind}","seq":{seq}}}"#),
    }
}

fn chat_session(storage: &Storage) -> SessionId {
    let ws = workspace("chat", 0);
    let mut row = session(ws.id, "codex");
    row.kind = SessionKind::Harness;
    storage.workspaces().insert(ws).unwrap();
    let id = row.id;
    storage.sessions().upsert(row).unwrap();
    id
}

#[test]
fn the_chat_journal_pages_backwards_and_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let id;
    {
        let storage = Storage::open(db(&dir)).unwrap();
        id = chat_session(&storage);
        assert_eq!(storage.chat().last_seq(id).unwrap(), 0);
        // seq has gaps where streaming deltas were.
        for seq in [1, 2, 5, 6, 9] {
            storage
                .chat()
                .append(chat_item(id, seq, "message_end"))
                .unwrap();
        }
        // A repeated seq is ignored, not an error and not an overwrite.
        let mut again = chat_item(id, 5, "other");
        again.event_json = "{}".to_owned();
        storage.chat().append(again).unwrap();
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.chat().last_seq(id).unwrap(), 9);
    let seqs = |before, limit| -> Vec<u64> {
        storage
            .chat()
            .items(id, before, limit)
            .unwrap()
            .iter()
            .map(|r| r.seq)
            .collect()
    };
    assert_eq!(seqs(None, 100), [1, 2, 5, 6, 9]);
    assert_eq!(seqs(None, 2), [6, 9], "the newest window, oldest first");
    assert_eq!(seqs(Some(6), 2), [2, 5]);
    assert_eq!(seqs(Some(1), 10), Vec::<u64>::new());
    let row = &storage.chat().items(id, Some(6), 1).unwrap()[0];
    assert_eq!(
        (row.kind.as_str(), row.turn_id.as_deref()),
        ("message_end", Some("turn-1"))
    );
    assert!(
        row.event_json.contains("\"seq\":5"),
        "the first write won: {}",
        row.event_json
    );
    storage.close();
}

#[test]
fn chat_turns_are_updated_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    let mut turn = ChatTurnRow {
        session_id: id,
        turn_id: "t1".to_owned(),
        provider_turn_id: Some("p1".to_owned()),
        status: ChatTurnStatus::Running,
        started_at_ms: 100,
        ended_at_ms: None,
        error: None,
    };
    storage.chat().upsert_turn(turn.clone()).unwrap();
    turn.status = ChatTurnStatus::Failed;
    turn.ended_at_ms = Some(150);
    turn.error = Some("boom".to_owned());
    turn.provider_turn_id = None;
    storage.chat().upsert_turn(turn.clone()).unwrap();
    let turns = storage.chat().turns(id).unwrap();
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].status, ChatTurnStatus::Failed);
    assert_eq!(turns[0].error.as_deref(), Some("boom"));
    assert_eq!(
        turns[0].provider_turn_id.as_deref(),
        Some("p1"),
        "a known provider id is kept"
    );
    storage.close();
}

#[test]
fn approvals_record_the_request_and_the_decision() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    let open = ApprovalRow {
        id: ApprovalId::new(),
        session_id: id,
        turn_id: Some("t1".to_owned()),
        kind: "command".to_owned(),
        title: "touch hi.txt".to_owned(),
        options_json: r#"[{"id":"accept"}]"#.to_owned(),
        requested_at_ms: 200,
        resolved_at_ms: None,
        outcome: None,
        option_id: None,
        decided_by: None,
    };
    let mut decided = open.clone();
    decided.id = ApprovalId::new();
    decided.requested_at_ms = 100;
    storage.approvals().upsert(open.clone()).unwrap();
    storage.approvals().upsert(decided.clone()).unwrap();
    assert_eq!(storage.approvals().unresolved().unwrap().len(), 2);

    decided.resolved_at_ms = Some(300);
    decided.outcome = Some(ApprovalOutcome::Selected);
    decided.option_id = Some("accept".to_owned());
    decided.decided_by = Some("cli".to_owned());
    storage.approvals().upsert(decided.clone()).unwrap();

    assert_eq!(
        storage.approvals().unresolved().unwrap(),
        std::slice::from_ref(&open)
    );
    assert_eq!(
        storage.approvals().list(Some(id)).unwrap(),
        [decided, open],
        "oldest request first, decision recorded"
    );
    assert!(
        storage
            .approvals()
            .list(Some(SessionId::new()))
            .unwrap()
            .is_empty()
    );
    storage.close();
}

#[test]
fn deleting_a_session_deletes_its_journal_and_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    storage
        .chat()
        .append(chat_item(id, 1, "turn_started"))
        .unwrap();
    storage
        .approvals()
        .upsert(ApprovalRow {
            id: ApprovalId::new(),
            session_id: id,
            turn_id: None,
            kind: "command".to_owned(),
            title: "x".to_owned(),
            options_json: "[]".to_owned(),
            requested_at_ms: 1,
            resolved_at_ms: None,
            outcome: None,
            option_id: None,
            decided_by: None,
        })
        .unwrap();
    storage.sessions().delete(id).unwrap();
    assert!(storage.chat().items(id, None, 10).unwrap().is_empty());
    assert!(storage.approvals().list(None).unwrap().is_empty());
    storage.close();
}

#[test]
fn migrating_v2_adds_the_chat_tables_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &[V1, V2]).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.close();
    }
    assert!(!table_exists(&db(&dir), "chat_items"));
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    storage.close();
    for table in ["chat_items", "chat_turns", "approvals"] {
        assert!(table_exists(&db(&dir), table), "{table}");
    }
    integrity_ok(&db(&dir));
}

#[test]
fn agent_trees_are_replaced_listed_and_removed_with_their_session() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let first = chat_session(&storage);
    let second = chat_session(&storage);
    assert!(storage.agent_trees().list().unwrap().is_empty());

    storage
        .agent_trees()
        .upsert(first, "{\"a\":1}".into(), 10)
        .unwrap();
    storage
        .agent_trees()
        .upsert(second, "{\"b\":2}".into(), 11)
        .unwrap();
    storage
        .agent_trees()
        .upsert(first, "{\"a\":3}".into(), 12)
        .unwrap();
    let mut listed = storage.agent_trees().list().unwrap();
    listed.sort_by_key(|(id, _)| id.to_string());
    let mut expected = vec![
        (first, "{\"a\":3}".to_owned()),
        (second, "{\"b\":2}".to_owned()),
    ];
    expected.sort_by_key(|(id, _)| id.to_string());
    assert_eq!(listed, expected);

    storage.agent_trees().delete(second).unwrap();
    assert_eq!(
        storage.agent_trees().list().unwrap(),
        [(first, "{\"a\":3}".to_owned())]
    );
    // A session's tree goes with the session.
    storage.sessions().delete(first).unwrap();
    assert!(storage.agent_trees().list().unwrap().is_empty());
    storage.close();
}

#[test]
fn migrating_v3_adds_the_agent_tree_table_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &migrate::MIGRATIONS[..3]).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.close();
    }
    assert!(!table_exists(&db(&dir), "agent_trees"));
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    storage.close();
    assert!(table_exists(&db(&dir), "agent_trees"));
    integrity_ok(&db(&dir));
}

#[test]
fn chat_events_can_be_read_by_kind() {
    let dir = tempfile::tempdir().unwrap();
    let storage = Storage::open(db(&dir)).unwrap();
    let id = chat_session(&storage);
    for (seq, kind) in [
        (1, "turn_started"),
        (2, "message_end"),
        (4, "message_end"),
        (5, "usage"),
        (6, "message_end"),
    ] {
        storage.chat().append(chat_item(id, seq, kind)).unwrap();
    }
    let all: Vec<u64> = storage
        .chat()
        .items_of_kind(id, "message_end", 10)
        .unwrap()
        .iter()
        .map(|r| r.seq)
        .collect();
    assert_eq!(all, [2, 4, 6]);
    let newest: Vec<u64> = storage
        .chat()
        .items_of_kind(id, "message_end", 2)
        .unwrap()
        .iter()
        .map(|r| r.seq)
        .collect();
    assert_eq!(newest, [4, 6], "the newest window, oldest first");
    assert!(
        storage
            .chat()
            .items_of_kind(id, "notice", 10)
            .unwrap()
            .is_empty()
    );
    storage.close();
}

fn provider(id: &str, created: u64) -> ProviderRow {
    ProviderRow {
        id: id.to_owned(),
        kind: "openai-compatible".to_owned(),
        name: format!("{id} name"),
        base_url: "http://127.0.0.1:1234/v1".to_owned(),
        default_model: Some("m1".to_owned()),
        keychain_ref: format!("dev.heeler.provider/{id}"),
        created_at_ms: created,
    }
}

#[test]
fn providers_are_stored_updated_and_deleted() {
    let dir = tempfile::tempdir().unwrap();
    {
        let storage = Storage::open(db(&dir)).unwrap();
        assert!(storage.providers().list().unwrap().is_empty());
        storage.providers().upsert(provider("b", 20)).unwrap();
        storage.providers().upsert(provider("a", 10)).unwrap();
        let mut changed = provider("b", 999);
        changed.default_model = None;
        changed.base_url = "http://elsewhere/v1".to_owned();
        storage.providers().upsert(changed).unwrap();
        storage.close();
    }
    let storage = Storage::open(db(&dir)).unwrap();
    let rows = storage.providers().list().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], provider("a", 10));
    assert_eq!(rows[1].id, "b");
    assert_eq!(rows[1].created_at_ms, 20, "the creation time is kept");
    assert_eq!(rows[1].default_model, None);
    assert_eq!(rows[1].base_url, "http://elsewhere/v1");
    storage.providers().delete("a").unwrap();
    storage.providers().delete("missing").unwrap();
    assert_eq!(storage.providers().list().unwrap().len(), 1);
    storage.close();
}

#[test]
fn migrating_v4_adds_the_provider_table_and_keeps_the_data() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &migrate::MIGRATIONS[..4]).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.close();
    }
    assert!(!table_exists(&db(&dir), "providers"));
    let storage = Storage::open(db(&dir)).unwrap();
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    assert_eq!(storage.app_meta().schema_version().unwrap(), 5);
    storage.close();
    assert!(table_exists(&db(&dir), "providers"));
    integrity_ok(&db(&dir));
}
