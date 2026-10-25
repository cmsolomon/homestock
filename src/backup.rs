//! Idle-aware online backup to an SMB-mounted NAS.
//!
//! - Dirty detection: `PRAGMA data_version` on our own long-lived connection changes
//!   whenever the app's connection commits. The initial value is a clean baseline, so
//!   startup alone does not trigger a backup. No bookkeeping in handlers.
//! - Idle detection: `Activity` (updated by middleware on every request).
//! - Safety: only runs if `<backup directory>/<sentinel>` exists, so an unmounted share
//!   (an empty local mount point) never fills the container disk.
use crate::{activity::Activity, config::BackupConfig, db::open_conn};
use rusqlite::{backup::Backup, Connection};
use std::{fs, path::{Path, PathBuf}, sync::Arc, time::{Duration, SystemTime, UNIX_EPOCH}};

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() }

pub struct Backupper {
    cfg: BackupConfig,
    db_path: PathBuf,
    data_dir: PathBuf,
    conn: Connection,
    last_version: Option<i64>,
    last_backup: u64,
}

impl Backupper {
    pub fn new(cfg: BackupConfig, db_path: PathBuf, data_dir: PathBuf) -> rusqlite::Result<Self> {
        let conn = Connection::open(&db_path)?;
        let last_version = conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        Ok(Self { cfg, db_path, data_dir, conn, last_version: Some(last_version), last_backup: 0 })
    }

    fn dirty(&mut self) -> rusqlite::Result<bool> {
        let v: i64 = self.conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        Ok(self.last_version != Some(v))
    }

    fn nas_ready(&self) -> bool { Path::new(&self.cfg.directory).join(&self.cfg.sentinel).exists() }

    /// Decide and (maybe) run. Called every 60 s on the blocking pool, and once at shutdown with `force`.
    pub fn tick(&mut self, activity: &Activity, force: bool) {
        let dirty = match self.dirty() { Ok(d) => d, Err(e) => { tracing::warn!("backup: data_version failed: {e}"); return; } };
        if !dirty { return; }
        let t = now();
        let idle = t.saturating_sub(activity.last_request()) >= self.cfg.idle_secs;
        let since = t.saturating_sub(self.last_backup);
        let due = force
            || (idle && since >= self.cfg.min_interval_secs)
            || since >= self.cfg.max_interval_secs;
        if !due { return; }
        if !self.nas_ready() { tracing::warn!("backup: sentinel missing at {}, skipping", self.cfg.directory); return; }

        match self.run() {
            Ok(name) => {
                self.last_backup = t;
                self.last_version = self.conn.query_row("PRAGMA data_version", [], |r| r.get(0)).ok();
                tracing::info!("backup: wrote {name}");
                if let Err(e) = self.prune() { tracing::warn!("backup: prune failed: {e}"); }
                if self.cfg.sync_photos { if let Err(e) = self.sync_photos() { tracing::warn!("backup: photo sync failed: {e}"); } }
                let _ = self.conn.execute_batch("PRAGMA optimize;");
            }
            Err(e) => tracing::error!("backup: failed: {e}"),
        }
    }

    fn run(&self) -> Result<String, Box<dyn std::error::Error>> {
        let name = format!("homestock-{}.sqlite3", now());
        let nas = PathBuf::from(&self.cfg.directory);
        let staging = if self.cfg.stage_locally { self.data_dir.join("staging") } else { nas.clone() };
        fs::create_dir_all(&staging)?;
        let tmp = staging.join(format!("{name}.part"));
        let _ = fs::remove_file(&tmp);

        // Online backup: copies 256 pages at a time and pauses between steps,
        // so app writes are never blocked for long.
        {
            let src = open_conn(&self.db_path).map_err(|e| format!("{e:?}"))?;
            let mut dst = Connection::open(&tmp)?;
            let b = Backup::new(&src, &mut dst)?;
            b.run_to_completion(256, Duration::from_millis(20), None)?;
            drop(b);
            let check: String = dst.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
            if check != "ok" { return Err(format!("quick_check: {check}").into()); }
        } // dst closed + flushed here

        let final_path = nas.join(&name);
        if self.cfg.stage_locally {
            let part = nas.join(format!("{name}.part"));
            fs::copy(&tmp, &part)?;
            fs::File::open(&part)?.sync_all()?;
            fs::rename(&part, &final_path)?;
            fs::remove_file(&tmp)?;
        } else {
            fs::rename(&tmp, &final_path)?;
        }
        Ok(name)
    }

    fn prune(&self) -> std::io::Result<()> {
        let mut files: Vec<_> = fs::read_dir(&self.cfg.directory)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).map_or(false, |n| n.starts_with("homestock-") && n.ends_with(".sqlite3")))
            .collect();
        files.sort(); // unix-timestamp names sort chronologically
        let excess = files.len().saturating_sub(self.cfg.keep);
        for p in files.into_iter().take(excess) { fs::remove_file(p)?; }
        Ok(())
    }

    /// Photos are content-addressed and immutable: copy whatever the NAS lacks.
    fn sync_photos(&self) -> std::io::Result<()> {
        let src = self.data_dir.join("photos");
        let dst = Path::new(&self.cfg.directory).join("photos");
        if !src.exists() { return Ok(()); }
        fs::create_dir_all(&dst)?;
        for e in fs::read_dir(&src)? {
            let p = e?.path();
            let Some(name) = p.file_name() else { continue };
            let target = dst.join(name);
            if !target.exists() {
                let part = dst.join(format!("{}.part", name.to_string_lossy()));
                fs::copy(&p, &part)?;
                fs::rename(&part, &target)?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Backupper;
    use crate::config::BackupConfig;
    use rusqlite::Connection;
    use std::{fs, time::{SystemTime, UNIX_EPOCH}};

    fn test_directory() -> std::path::PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).expect("clock").as_nanos();
        let dir = std::env::temp_dir().join(format!("homestock-backup-test-{suffix}"));
        fs::create_dir_all(&dir).expect("create test directory");
        dir
    }

    fn test_config(directory: String) -> BackupConfig {
        BackupConfig {
            directory,
            sentinel: ".homestock-nas".into(),
            idle_secs: 300,
            min_interval_secs: 1800,
            max_interval_secs: 86400,
            keep: 48,
            stage_locally: true,
            sync_photos: true,
        }
    }

    #[test]
    fn clean_start_is_not_dirty_but_external_commit_is() {
        let dir = test_directory();
        let db_path = dir.join("homestock.sqlite3");
        let other_connection = Connection::open(&db_path).expect("open database");
        other_connection.execute_batch("CREATE TABLE sample (value INTEGER);").expect("create table");

        let mut backupper = Backupper::new(
            test_config(dir.join("backups").to_string_lossy().into_owned()),
            db_path,
            dir.clone(),
        ).expect("create backupper");

        assert!(!backupper.dirty().expect("check initial database version"));

        other_connection.execute("INSERT INTO sample(value) VALUES (1)", []).expect("commit change");
        assert!(backupper.dirty().expect("detect external database change"));

        drop(backupper);
        drop(other_connection);
        fs::remove_dir_all(dir).expect("remove test directory");
    }
}

/// Spawned from main. Wakes once a minute; nearly free when idle.
pub async fn run_loop(mut b: Backupper, activity: Arc<Activity>, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut iv = tokio::time::interval(Duration::from_secs(60));
    iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            _ = iv.tick() => {
                let a = activity.clone();
                b = tokio::task::spawn_blocking(move || { b.tick(&a, false); b }).await.expect("backup task panicked");
            }
            _ = shutdown.changed() => {
                let a = activity.clone();
                let _ = tokio::task::spawn_blocking(move || b.tick(&a, true)).await;
                break;
            }
        }
    }
}
