//! Forward-only embedded migrations with a pre-migration backup (ADR 0006).
//!
//! The schema version lives in `app_meta` (`schema_version`). Pending migrations run one per
//! transaction together with the version bump, so a failing migration rolls back completely and
//! the database stays at the last good version.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension};
use tracing::{info, warn};

use crate::error::{Result, StorageError};

pub const SCHEMA_VERSION_KEY: &str = "schema_version";

/// Backups kept in `backups/`; older ones are deleted after a new one is made.
pub const BACKUPS_KEPT: usize = 5;

#[derive(Debug, Clone, Copy)]
pub struct Migration {
    /// 1-based and consecutive.
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// Migrations shipped in this build.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "projects",
        sql: include_str!("../migrations/0002_projects.sql"),
    },
];

/// Schema version recorded in the database; 0 for a database without `app_meta`.
pub fn current_version(conn: &Connection) -> Result<u32> {
    let context = "reading the schema version";
    let has_meta: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'app_meta')",
            [],
            |row| row.get(0),
        )
        .map_err(StorageError::sqlite(context))?;
    if !has_meta {
        return Ok(0);
    }
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM app_meta WHERE key = ?1",
            [SCHEMA_VERSION_KEY],
            |row| row.get(0),
        )
        .optional()
        .map_err(StorageError::sqlite(context))?;
    match value {
        None => Ok(0),
        Some(text) => text
            .parse()
            .map_err(|_| StorageError::corrupt("app_meta.schema_version", format!("`{text}`"))),
    }
}

/// Brings the database at `path` up to the newest of `migrations`, backing it up first when
/// there is anything to migrate.
pub fn run(conn: &mut Connection, path: &Path, migrations: &[Migration]) -> Result<()> {
    let supported = migrations.last().map_or(0, |m| m.version);
    debug_assert!(
        migrations
            .iter()
            .zip(1..)
            .all(|(migration, expected)| migration.version == expected),
        "migrations must be numbered 1..n without gaps"
    );
    let from = current_version(conn)?;
    if from > supported {
        return Err(StorageError::SchemaTooNew {
            path: path.to_owned(),
            found: from,
            supported,
        });
    }
    if from == supported {
        return Ok(());
    }

    // A fresh database has nothing worth keeping.
    let backup = if from > 0 {
        Some(backup(conn, path, from)?)
    } else {
        None
    };
    for migration in migrations.iter().filter(|m| m.version > from) {
        apply(conn, migration).map_err(|source| StorageError::Migration {
            version: migration.version,
            name: migration.name.to_owned(),
            backup: backup.clone(),
            source,
        })?;
        info!(
            version = migration.version,
            name = migration.name,
            "migrated"
        );
    }
    Ok(())
}

fn apply(conn: &mut Connection, migration: &Migration) -> rusqlite::Result<()> {
    let tx = conn.transaction()?;
    tx.execute_batch(migration.sql)?;
    tx.execute(
        "INSERT INTO app_meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        (SCHEMA_VERSION_KEY, migration.version.to_string()),
    )?;
    tx.commit()
}

/// Consistent copy (`VACUUM INTO`, so WAL content is included) in `backups/` next to the
/// database; keeps the newest [`BACKUPS_KEPT`].
fn backup(conn: &Connection, path: &Path, from: u32) -> Result<PathBuf> {
    let dir = path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("backups");
    fs::create_dir_all(&dir).map_err(StorageError::io(format!(
        "creating backup directory {}",
        dir.display()
    )))?;
    let mut stamp = now_ms();
    let target = loop {
        let candidate = dir.join(backup_name(stamp, from));
        if !candidate.exists() {
            break candidate;
        }
        stamp += 1;
    };
    conn.execute("VACUUM INTO ?1", [target.to_string_lossy()])
        .map_err(StorageError::sqlite(format!(
            "backing up the database to {}",
            target.display()
        )))?;
    info!(backup = %target.display(), from, "database backed up before migrating");
    prune_backups(&dir);
    Ok(target)
}

fn backup_name(stamp_ms: u64, from: u32) -> String {
    // Zero-padded so names sort chronologically.
    format!("heeler-{stamp_ms:013}-v{from}.db")
}

fn backup_stamp(name: &str) -> Option<u64> {
    name.strip_prefix("heeler-")?
        .strip_suffix(".db")?
        .split_once("-v")?
        .0
        .parse()
        .ok()
}

fn prune_backups(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    let mut backups: Vec<(u64, PathBuf)> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let stamp = backup_stamp(&entry.file_name().to_string_lossy())?;
            Some((stamp, entry.path()))
        })
        .collect();
    backups.sort();
    let excess = backups.len().saturating_sub(BACKUPS_KEPT);
    for (_, old) in backups.drain(..excess) {
        if let Err(error) = fs::remove_file(&old) {
            warn!(backup = %old.display(), %error, "could not remove an old backup");
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_names_roundtrip_and_sort_by_time() {
        let name = backup_name(1_791_894_164_332, 3);
        assert_eq!(name, "heeler-1791894164332-v3.db");
        assert_eq!(backup_stamp(&name), Some(1_791_894_164_332));
        assert!(backup_name(999, 1) < backup_name(1_000, 1));
        assert_eq!(backup_stamp("other.db"), None);
    }
}
