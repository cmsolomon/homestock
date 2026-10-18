use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub bind: String,
    pub data_dir: PathBuf,
    #[serde(default)]
    pub database_path: Option<PathBuf>,
    #[serde(default = "d30")] pub session_days: i64,
    #[serde(default = "yes")] pub cookie_secure: bool,
    pub backup: BackupConfig,
    #[serde(default)] pub seed_users: Vec<SeedUser>,
}
fn d30() -> i64 { 30 }

#[derive(Deserialize, Clone)]
pub struct BackupConfig {
    #[serde(alias = "nas_dir")]
    pub directory: String,
    #[serde(default = "sent")] pub sentinel: String,
    #[serde(default = "s300")] pub idle_secs: u64,
    #[serde(default = "s1800")] pub min_interval_secs: u64,
    #[serde(default = "s86400")] pub max_interval_secs: u64,
    #[serde(default = "k48")] pub keep: usize,
    #[serde(default = "yes")] pub stage_locally: bool,
    #[serde(default = "yes")] pub sync_photos: bool,
}
fn sent() -> String { ".homestock-nas".into() }
fn s300() -> u64 { 300 } fn s1800() -> u64 { 1800 } fn s86400() -> u64 { 86400 } fn k48() -> usize { 48 } fn yes() -> bool { true }

#[derive(Deserialize, Clone)]
pub struct SeedUser { pub name: String, #[serde(default = "member")] pub role: String, pub password: String }
fn member() -> String { "member".into() }

impl Config {
    pub fn database_path(&self) -> PathBuf {
        self.database_path.clone().unwrap_or_else(|| self.data_dir.join("homestock.sqlite3"))
    }

    /// `homestock --config /etc/homestock/config.toml` (default ./config.toml)
    pub fn load_from_args() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut args = std::env::args().skip(1);
        let mut path = PathBuf::from("config.toml");
        while let Some(a) = args.next() { if a == "--config" { if let Some(p) = args.next() { path = p.into(); } } }
        Ok(toml::from_str(&std::fs::read_to_string(&path)?)?)
    }
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn config_supports_explicit_database_and_backup_paths() {
        let config: Config = toml::from_str(
            r#"
            bind = "0.0.0.0:8080"
            data_dir = "/data"
            database_path = "/data/inventory.sqlite3"
            [backup]
            directory = "/backups"
            "#,
        ).expect("parse config with explicit paths");

        assert_eq!(config.database_path().as_path(), std::path::Path::new("/data/inventory.sqlite3"));
        assert_eq!(config.backup.directory, "/backups");
    }

    #[test]
    fn config_keeps_compatibility_with_legacy_database_and_backup_paths() {
        let config: Config = toml::from_str(
            r#"
            bind = "0.0.0.0:8080"
            data_dir = "/data"
            [backup]
            nas_dir = "/nas"
            "#,
        ).expect("parse legacy config");

        assert_eq!(config.database_path().as_path(), std::path::Path::new("/data/homestock.sqlite3"));
        assert_eq!(config.backup.directory, "/nas");
    }
}
