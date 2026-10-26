use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub bind: String,
    #[serde(default)]
    pub tls_cert: Option<PathBuf>,
    #[serde(default)]
    pub tls_key: Option<PathBuf>,
    pub data_dir: PathBuf,
    #[serde(default)]
    pub database_path: Option<PathBuf>,
    #[serde(default = "d30")]
    pub session_days: i64,
    #[serde(default = "yes")]
    pub cookie_secure: bool,
    pub backup: BackupConfig,
    #[serde(default)]
    pub seed_users: Vec<SeedUser>,
    /// Reverse proxies (e.g. HAProxy) whose X-Forwarded-For is trusted for the client address.
    #[serde(default)]
    pub trusted_proxies: Vec<std::net::IpAddr>,
}
fn d30() -> i64 {
    30
}

#[derive(Deserialize, Clone)]
pub struct BackupConfig {
    #[serde(default = "s300")]
    pub idle_secs: u64,
    #[serde(default = "s1800")]
    pub min_interval_secs: u64,
    #[serde(default = "s86400")]
    pub max_interval_secs: u64,
    #[serde(default = "k3")]
    pub pending_keep: usize,
}
fn s300() -> u64 {
    300
}
fn s1800() -> u64 {
    1800
}
fn s86400() -> u64 {
    86400
}
fn k3() -> usize {
    3
}
fn yes() -> bool {
    true
}

#[derive(Deserialize, Clone)]
pub struct SeedUser {
    pub name: String,
    #[serde(default = "member")]
    pub role: String,
    pub password: String,
}
fn member() -> String {
    "member".into()
}

impl Config {
    pub fn database_path(&self) -> PathBuf {
        self.database_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join("homestock.sqlite3"))
    }

    /// `homestock --config /etc/homestock/config.toml` (default ./config.toml)
    pub fn load_from_args() -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        let mut args = std::env::args().skip(1);
        let mut path = PathBuf::from("config.toml");
        while let Some(a) = args.next() {
            if a == "--config" {
                if let Some(p) = args.next() {
                    path = p.into();
                }
            }
        }
        let cfg: Self = toml::from_str(&std::fs::read_to_string(&path)?)?;
        if cfg.session_days < 1 {
            return Err("session_days must be at least 1".into());
        }
        Ok(cfg)
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
            tls_cert = "/certs/cert.pem"
            tls_key = "/certs/key.pem"
            data_dir = "/data"
            database_path = "/data/inventory.sqlite3"
            [backup]
            pending_keep = 3
            "#,
        )
        .expect("parse config with explicit paths");

        assert_eq!(
            config.database_path().as_path(),
            std::path::Path::new("/data/inventory.sqlite3")
        );
        assert_eq!(
            config.tls_cert.as_deref(),
            Some(std::path::Path::new("/certs/cert.pem"))
        );
        assert_eq!(
            config.tls_key.as_deref(),
            Some(std::path::Path::new("/certs/key.pem"))
        );
        assert_eq!(config.backup.pending_keep, 3);
    }

    #[test]
    fn config_keeps_compatibility_with_legacy_database_and_backup_paths() {
        let config: Config = toml::from_str(
            r#"
            bind = "0.0.0.0:8080"
            data_dir = "/data"
            [backup]
            directory = "/nas"
            "#,
        )
        .expect("parse legacy config");

        assert_eq!(
            config.database_path().as_path(),
            std::path::Path::new("/data/homestock.sqlite3")
        );
        assert!(config.tls_cert.is_none());
        assert!(config.tls_key.is_none());
        assert_eq!(config.backup.pending_keep, 3);
    }

    #[test]
    fn trusted_proxies_are_parsed_and_invalid_addresses_rejected() {
        let base = "bind = \"0.0.0.0:8443\"\ndata_dir = \"/data\"\n[backup]\n";
        let config: Config = toml::from_str(&format!(
            "trusted_proxies = [\"10.0.10.1\", \"fd00::1\"]\n{base}"
        ))
        .expect("parse trusted proxies");
        assert_eq!(
            config.trusted_proxies,
            [
                "10.0.10.1".parse::<std::net::IpAddr>().unwrap(),
                "fd00::1".parse().unwrap()
            ]
        );
        assert!(toml::from_str::<Config>(base)
            .expect("parse without proxies")
            .trusted_proxies
            .is_empty());
        assert!(
            toml::from_str::<Config>(&format!("trusted_proxies = [\"opnsense.lan\"]\n{base}"))
                .is_err()
        );
    }
}
