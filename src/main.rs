mod activity;
mod auth;
mod backup;
mod config;
mod db;
mod error;
mod photos;
mod repo;
mod routes;
mod search;
mod security;

use std::{net::SocketAddr, sync::Arc, time::Duration};

#[derive(Clone)]
pub struct AppState {
    pub db: db::Db,
    pub cfg: Arc<config::Config>,
    pub activity: Arc<activity::Activity>,
    pub passwords: Arc<security::PasswordLimiter>,
}

// current_thread: one OS thread for async work; blocking work uses tokio's lazy blocking pool.
#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow_free::Result {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    let cfg = Arc::new(config::Config::load_from_args()?);
    std::fs::create_dir_all(cfg.data_dir.join("photos"))?;
    let db_path = cfg.database_path();
    if let Some(parent) = db_path.parent().filter(|path| !path.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let db = db::Db::open(&db_path)?;
    {
        let seeds = cfg.seed_users.clone();
        db.call(move |c| auth::seed_users(c, &seeds)).await?;
    }

    let activity = Arc::new(activity::Activity::default());
    let state = AppState {
        db,
        cfg: cfg.clone(),
        activity: activity.clone(),
        passwords: Default::default(),
    };

    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let backupper =
        backup::Backupper::new(cfg.backup.clone(), db_path.clone(), cfg.data_dir.clone())?;
    let backup_task = tokio::spawn(backup::run_loop(backupper, activity.clone(), stop_rx));

    let app = routes::router(state);
    let addr: SocketAddr = cfg.bind.parse()?;
    let handle = axum_server::Handle::new();
    let shutdown_handle = handle.clone();
    tokio::spawn(async move {
        if let Err(error) = shutdown_signal().await {
            tracing::error!("failed to install shutdown signal handler: {error}");
        }
        shutdown_handle.graceful_shutdown(Some(Duration::from_secs(30)));
    });

    match (&cfg.tls_cert, &cfg.tls_key) {
        (Some(cert), Some(key)) => {
            let tls = axum_server::tls_rustls::RustlsConfig::from_pem_file(cert, key).await?;
            tracing::info!("listening on https://{addr}");
            axum_server::tls_rustls::bind_rustls(addr, tls)
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
        (None, None) => {
            tracing::info!("listening on http://{addr}");
            axum_server::bind(addr)
                .handle(handle)
                .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                .await?;
        }
        _ => return Err("tls_cert and tls_key must be set together".into()),
    }

    let _ = stop_tx.send(true); // final backup if dirty
    let _ = backup_task.await;
    Ok(())
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result?,
            _ = terminate.recv() => {}
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await
    }
}

/// Tiny boxed-error alias for `main`, avoiding an `anyhow` dependency.
mod anyhow_free {
    pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;
}
