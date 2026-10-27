use crate::error::AppError;
use rusqlite::{backup::Backup, Connection};
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

// Keep this in sync with the highest PRAGMA user_version in migrations/.
pub const CURRENT_SCHEMA_VERSION: i64 = 2;

/// Single connection behind a mutex. Every call runs on the blocking pool.
/// For a 2–6 person household this is simpler and lighter than a pool,
/// and SQLite serialises writers anyway.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let existed = path.exists();
        let conn = open_conn(path)?;
        let version = schema_version(&conn)?;
        if version > CURRENT_SCHEMA_VERSION {
            return Err(newer_schema(version));
        }
        if existed && version < CURRENT_SCHEMA_VERSION {
            let backup = backup_before_migration(&conn, path, version, CURRENT_SCHEMA_VERSION)?;
            tracing::info!(path = %backup.display(), from_version = version, to_version = CURRENT_SCHEMA_VERSION, "saved database before schema migration");
        }
        migrate(&conn)?;
        Ok(Db(Arc::new(Mutex::new(conn))))
    }

    pub async fn call<F, T>(&self, f: F) -> Result<T, AppError>
    where
        F: FnOnce(&mut Connection) -> Result<T, AppError> + Send + 'static,
        T: Send + 'static,
    {
        let inner = self.0.clone();
        tokio::task::spawn_blocking(move || {
            let mut conn = inner
                .lock()
                .map_err(|_| AppError::internal("db mutex poisoned"))?;
            f(&mut conn)
        })
        .await
        .map_err(|e| AppError::internal(e.to_string()))?
    }
}

pub fn open_conn(path: &Path) -> Result<Connection, AppError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    debug_assert_eq!(mode.to_lowercase(), "wal");
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA temp_store = MEMORY;
         PRAGMA cache_size = -4000;",
    )?;
    Ok(conn)
}

fn migrate(conn: &Connection) -> Result<(), AppError> {
    let v = schema_version(conn)?;
    if v < 1 {
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
    }
    if v < 2 {
        conn.execute_batch(include_str!("../migrations/0002_soft_delete_users.sql"))?;
    }
    Ok(())
}

pub fn schema_version(conn: &Connection) -> Result<i64, AppError> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// A database migrated by a newer release: running this build on it could corrupt data
/// the newer schema relies on, so refuse instead.
pub fn newer_schema(version: i64) -> AppError {
    AppError::internal(format!(
        "the database is at schema version {version}, but this Homestock build supports up to \
         {CURRENT_SCHEMA_VERSION}; install a newer release, or restore a backup taken before the upgrade"
    ))
}

fn backup_before_migration(
    conn: &Connection,
    path: &Path,
    from: i64,
    to: i64,
) -> Result<PathBuf, AppError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let filename = path
        .file_name()
        .ok_or_else(|| AppError::internal("database path has no file name"))?
        .to_string_lossy();
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| AppError::internal(format!("system clock is before Unix epoch: {e}")))?
        .as_nanos();
    let backup_path = parent.join(format!(
        "{filename}.pre-migration-v{from}-to-v{to}-{timestamp}.sqlite3"
    ));
    backup_to(conn, &backup_path, Verify::Quick)?;
    Ok(backup_path)
}

/// How thoroughly `backup_to` checks the copy before keeping it.
#[derive(Clone, Copy)]
pub enum Verify {
    /// `PRAGMA quick_check`: structure only; fast enough to run before every migration.
    Quick,
    /// `PRAGMA integrity_check`: also checks indexes against their tables.
    Full,
}

/// Online backup of `conn` to a new file at `dest`, which must not exist. The copy is
/// written to `<dest>.part` (mode 0600 on Unix), verified, synced and only then renamed
/// into place, so `dest` is either absent or a complete, checked database.
pub fn backup_to(conn: &Connection, dest: &Path, verify: Verify) -> Result<(), AppError> {
    if dest.exists() {
        return Err(AppError::conflict(format!(
            "{} already exists",
            dest.display()
        )));
    }
    let parent = dest
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = dest.as_os_str().to_owned();
    temporary.push(".part");
    let temporary_path = PathBuf::from(temporary);
    // A leftover from an interrupted run is ours (same name) and incomplete.
    match fs::remove_file(&temporary_path) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    drop(options.open(&temporary_path)?);

    let result = (|| -> Result<(), AppError> {
        let mut destination = Connection::open(&temporary_path)?;
        let backup = Backup::new(conn, &mut destination)?;
        backup.run_to_completion(256, Duration::from_millis(20), None)?;
        drop(backup);

        let pragma = match verify {
            Verify::Quick => "PRAGMA quick_check",
            Verify::Full => "PRAGMA integrity_check",
        };
        let check: String = destination.query_row(pragma, [], |r| r.get(0))?;
        if check != "ok" {
            return Err(AppError::internal(format!(
                "database backup failed its integrity check: {check}"
            )));
        }
        drop(destination);

        fs::File::open(&temporary_path)?.sync_all()?;
        fs::rename(&temporary_path, dest)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();

    if let Err(error) = result {
        if let Err(cleanup_error) = fs::remove_file(&temporary_path) {
            if cleanup_error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(path = %temporary_path.display(), error = %cleanup_error, "failed to remove incomplete database backup");
            }
        }
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{migrate, Db};
    use rusqlite::Connection;
    use std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn migration_adds_deleted_at_to_existing_user_table() {
        let conn = Connection::open_in_memory().expect("open in-memory database");
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))
            .expect("apply schema v1");
        conn.execute(
            "INSERT INTO user(name,role,password_hash) VALUES ('Alex','admin','hash')",
            [],
        )
        .expect("insert existing user");

        migrate(&conn).expect("apply latest migrations");

        let deleted_at: Option<i64> = conn
            .query_row("SELECT deleted_at FROM user WHERE name='Alex'", [], |row| {
                row.get(0)
            })
            .expect("read migrated user");
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read schema version");
        assert_eq!(deleted_at, None);
        assert_eq!(version, 2);
    }

    #[test]
    fn opening_older_database_saves_valid_backup_before_migrating() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("homestock-migration-test-{suffix}"));
        fs::create_dir_all(&dir).expect("create test directory");
        let path = dir.join("homestock.sqlite3");

        let old = Connection::open(&path).expect("create old database");
        old.execute_batch(include_str!("../migrations/0001_init.sql"))
            .expect("apply schema v1");
        old.execute(
            "INSERT INTO user(name,role,password_hash) VALUES ('Alex','admin','existing-hash')",
            [],
        )
        .expect("insert existing user");
        drop(old);

        let db = Db::open(&path).expect("open and migrate old database");
        let migrated_version =
            db.0.lock()
                .expect("lock migrated database")
                .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .expect("read migrated schema version");
        assert_eq!(migrated_version, 2);

        let backup_path: PathBuf = fs::read_dir(&dir)
            .expect("list test directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|candidate| {
                candidate
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.contains(".pre-migration-v1-to-v2-") && name.ends_with(".sqlite3")
                    })
            })
            .expect("migration backup should exist");
        let backup = Connection::open(&backup_path).expect("open pre-migration backup");
        let backup_version: i64 = backup
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .expect("read backup schema version");
        let user_hash: String = backup
            .query_row(
                "SELECT password_hash FROM user WHERE name='Alex'",
                [],
                |row| row.get(0),
            )
            .expect("read preserved user from backup");
        assert_eq!(backup_version, 1);
        assert_eq!(user_hash, "existing-hash");

        drop(backup);
        drop(db);
        fs::remove_dir_all(dir).expect("remove test directory");
    }
}
