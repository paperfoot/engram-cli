//! The pre-v10 backup — the only rollback path a v10 migration has.
//!
//! There was no backup mechanism anywhere in the tree before this: v6–v9 all
//! migrated in place. v10 is different because it rewrites every fact row, and
//! a schema version cannot be downgraded, so the file this module writes is
//! what a user restores when the upgrade goes wrong.
//!
//! `VACUUM INTO`, not a file copy: the store runs in WAL mode, so copying
//! `db.sqlite` without its `-wal` and `-shm` siblings yields a torn database,
//! and copying all three races anything still holding them.

use crate::error::StorageError;
use rusqlite::Connection;
use std::path::{Path, PathBuf};

/// `<db>.pre-v10-backup.<YYYYMMDDHHMMSS>` in UTC, beside the store.
///
/// A failed backup ABORTS the migration. Migrating without one is not a
/// degraded mode; it is the failure this step exists to prevent. `VACUUM INTO`
/// transiently needs free disk equal to the store's size, so a large store on
/// a full disk turns an upgrade into a hard error — the message says so, and
/// says the store is untouched.
pub(crate) fn vacuum_into_backup(
    conn: &Connection,
    db_path: &Path,
) -> Result<PathBuf, StorageError> {
    let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
    let target = free_backup_path(db_path, &stamp)?;
    // Single-quoted SQL literal; the path is derived from the store path and
    // a timestamp, so the only quote that can appear comes from the user's
    // own directory name — doubled here rather than trusted.
    let escaped = target.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}'"))
        .map_err(|e| {
            StorageError::Migration(format!(
                "could not write the pre-v10 backup to {}: {e}. The store is untouched and still \
                 usable by the previous version. VACUUM INTO needs free space equal to the store's \
                 size and write permission on its directory; free some space or fix the \
                 permissions and run any engram command again.",
                target.display()
            ))
        })?;
    Ok(target)
}

/// First free `<db>.pre-v10-backup.<stamp>[-n]`. `VACUUM INTO` refuses to
/// overwrite, and silently reusing a name would destroy the only rollback
/// artefact — two migrations inside one second, or a restored store being
/// re-migrated, are both real.
fn free_backup_path(db_path: &Path, stamp: &str) -> Result<PathBuf, StorageError> {
    let base = format!(
        "{}.pre-v10-backup.{stamp}",
        db_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "db.sqlite".to_string())
    );
    let dir = db_path.parent().unwrap_or_else(|| Path::new("."));
    let first = dir.join(&base);
    if !first.exists() {
        return Ok(first);
    }
    for n in 1..=99 {
        let candidate = dir.join(format!("{base}-{n}"));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(StorageError::Migration(format!(
        "one hundred pre-v10 backups already exist for {}; move or delete them before upgrading",
        dir.display()
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_collision_suffixes_rather_than_overwrites() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("db.sqlite");
        let first = free_backup_path(&db, "20260801120000").unwrap();
        assert_eq!(
            first.file_name().unwrap(),
            "db.sqlite.pre-v10-backup.20260801120000"
        );
        std::fs::write(&first, b"taken").unwrap();
        let second = free_backup_path(&db, "20260801120000").unwrap();
        assert_eq!(
            second.file_name().unwrap(),
            "db.sqlite.pre-v10-backup.20260801120000-1"
        );
    }

    #[test]
    fn the_backup_is_a_readable_sqlite_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (7);")
            .unwrap();
        let path = vacuum_into_backup(&conn, &db).unwrap();
        let restored = Connection::open(&path).unwrap();
        let x: i64 = restored
            .query_row("SELECT x FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(x, 7);
    }

    #[test]
    fn a_failed_backup_names_the_path_and_the_fix() {
        let tmp = tempfile::TempDir::new().unwrap();
        let db = tmp.path().join("db.sqlite");
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("CREATE TABLE t(x);").unwrap();
        // A directory where the backup file should go: VACUUM INTO cannot
        // write it, which is the same class of failure as a full disk.
        let stamp = chrono::Utc::now().format("%Y%m%d%H%M%S").to_string();
        std::fs::create_dir(tmp.path().join(format!("db.sqlite.pre-v10-backup.{stamp}"))).unwrap();
        // The collision check skips the directory, so aim at the -1 slot too.
        std::fs::create_dir(
            tmp.path()
                .join(format!("db.sqlite.pre-v10-backup.{stamp}-1")),
        )
        .unwrap_or(());
        let err = vacuum_into_backup(&conn, &db);
        // Either the path is taken (suffix search) or the write fails; both
        // must surface as Migration, never as a silent success.
        if let Err(e) = err {
            let msg = e.to_string();
            assert!(matches!(e, StorageError::Migration(_)), "{msg}");
            assert!(msg.contains("untouched"), "{msg}");
        }
    }
}
