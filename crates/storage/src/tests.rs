#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::fs;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use heeler_core::{Epoch, SessionId, SessionKind, SessionState, WorkspaceId};
use tempfile::TempDir;

use crate::migrate::Migration;
use crate::sql::Fault;
use crate::{SessionRow, Storage, StorageError, WorkspaceRow, Write, migrate};

fn db(dir: &TempDir) -> PathBuf {
    dir.path().join("heeler.db")
}

fn workspace(name: &str, position: i64) -> WorkspaceRow {
    WorkspaceRow {
        id: WorkspaceId::new(),
        name: name.to_owned(),
        roots: vec![PathBuf::from("/work/a"), PathBuf::from("/work/b")],
        created_at_ms: 1_000 + u64::try_from(position).unwrap(),
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
const V2_OK: Migration = Migration {
    version: 2,
    name: "extra",
    sql: "CREATE TABLE extra (x INTEGER NOT NULL);",
};
const V2_BROKEN: Migration = Migration {
    version: 2,
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

    assert_eq!(storage.app_meta().schema_version().unwrap(), 1);
    assert!(storage.workspaces().list().unwrap().is_empty());
    assert!(storage.sessions().list().unwrap().is_empty());
    assert!(backups(&dir).is_empty(), "a fresh database needs no backup");
    storage.close();

    for table in ["app_meta", "workspaces", "sessions"] {
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
        assert_eq!(storage.app_meta().schema_version().unwrap(), 1);
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
        let storage = Storage::open_with_migrations(db(&dir), &[V1]).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.close();
    }
    let storage = Storage::open_with_migrations(db(&dir), &[V1, V2_OK]).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), 2);
    storage.close();
    assert!(table_exists(&db(&dir), "extra"));

    let files = backups(&dir);
    assert_eq!(files.len(), 1, "{files:?}");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        name.starts_with("heeler-") && name.ends_with("-v1.db"),
        "{name}"
    );
    // The backup is the pre-migration database: version 1, data present, no `extra` table.
    assert!(!table_exists(&files[0], "extra"));
    let old = Storage::open_with_migrations(&files[0], &[V1]).unwrap();
    assert_eq!(old.workspaces().list().unwrap(), [ws]);
    old.close();
}

#[test]
fn a_failed_migration_keeps_the_original_and_reports_it() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace("alpha", 0);
    {
        let storage = Storage::open_with_migrations(db(&dir), &[V1]).unwrap();
        storage.workspaces().insert(ws.clone()).unwrap();
        storage.close();
    }

    let error = Storage::open_with_migrations(db(&dir), &[V1, V2_BROKEN]).unwrap_err();
    let StorageError::Migration {
        version,
        name,
        backup,
        ..
    } = &error
    else {
        panic!("expected a migration error, got {error}");
    };
    assert_eq!((*version, name.as_str()), (2, "broken"));
    assert!(backup.as_ref().is_some_and(|b| b.exists()));
    let text = error.to_string();
    assert!(text.contains("migration 2 (broken) failed"), "{text}");
    assert!(text.contains("left unchanged"), "{text}");

    // Nothing of the failed migration is visible; the data and version are as before.
    assert!(!table_exists(&db(&dir), "extra"));
    let storage = Storage::open_with_migrations(db(&dir), &[V1]).unwrap();
    assert_eq!(storage.app_meta().schema_version().unwrap(), 1);
    assert_eq!(storage.workspaces().list().unwrap(), [ws]);
    storage.close();
    integrity_ok(&db(&dir));
}

#[test]
fn a_database_from_a_newer_build_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    Storage::open_with_migrations(db(&dir), &[V1, V2_OK])
        .unwrap()
        .close();
    let error = Storage::open_with_migrations(db(&dir), &[V1]).unwrap_err();
    assert!(
        matches!(
            error,
            StorageError::SchemaTooNew {
                found: 2,
                supported: 1,
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn only_the_newest_five_backups_are_kept() {
    let dir = tempfile::tempdir().unwrap();
    Storage::open_with_migrations(db(&dir), &[V1])
        .unwrap()
        .close();
    let backup_dir = dir.path().join("backups");
    fs::create_dir_all(&backup_dir).unwrap();
    for stamp in 1..=7_u64 {
        fs::write(backup_dir.join(format!("heeler-{stamp:013}-v1.db")), b"old").unwrap();
    }
    fs::write(backup_dir.join("notes.txt"), b"not a backup").unwrap();

    Storage::open_with_migrations(db(&dir), &[V1, V2_OK])
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
