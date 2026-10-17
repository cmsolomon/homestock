use crate::error::AppError;
use rusqlite::Connection;
use std::{path::Path, sync::{Arc, Mutex}, time::Duration};

/// Single connection behind a mutex. Every call runs on the blocking pool.
/// For a 2–6 person household this is simpler and lighter than a pool,
/// and SQLite serialises writers anyway.
#[derive(Clone)]
pub struct Db(Arc<Mutex<Connection>>);

impl Db {
    pub fn open(path: &Path) -> Result<Self, AppError> {
        let conn = open_conn(path)?;
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
            let mut conn = inner.lock().map_err(|_| AppError::internal("db mutex poisoned"))?;
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
    let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    if v < 1 {
        conn.execute_batch(include_str!("../migrations/0001_init.sql"))?;
    }
    // if v < 2 { conn.execute_batch(include_str!("../migrations/0002_….sql"))?; }
    Ok(())
}
