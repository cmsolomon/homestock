//! Idle-aware online backup to a CIFS-mounted NAS.
//!
//! - Dirty detection: `PRAGMA data_version` on our own long-lived connection changes
//!   whenever the app's connection commits. No bookkeeping in handlers.
//! - Idle detection: `Activity` (updated by middleware on every request).
//! - Safety: only runs if `<nas_dir>/<sentinel>` exists, so an unmounted share
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
        Ok(Self { cfg, db_path, data_dir, conn, last_version: None, last_backup: 0 })
    }

    fn dirty(&mut self) -> rusqlite::Result<bool> {
        let v: i64 = self.conn.query_row("PRAGMA data_version", [], |r| r.get(0))?;
        Ok(self.last_version != Some(v))
    }

    fn nas_ready(&self) -> bool { Path::new(&self.cfg.nas_dir).join(&self.cfg.sentinel).exists() }

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
        if !self.nas_ready() { tracing::warn!("backup: NAS sentinel missing at {}, skipping", self.cfg.nas_dir); return; }

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
        let nas = PathBuf::from(&self.cfg.nas_dir);
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
        let mut files: Vec<_> = fs::read_dir(&self.cfg.nas_dir)?
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
        let dst = Path::new(&self.cfg.nas_dir).join("photos");
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
