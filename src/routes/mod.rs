use crate::{activity, AppState};
use axum::http::{header, HeaderValue};
use axum::{
    extract::DefaultBodyLimit,
    middleware,
    routing::{get, post},
    Router,
};
use tower_http::{
    compression::CompressionLayer, services::ServeDir, set_header::SetResponseHeaderLayer,
};

pub mod account;
pub mod auth;
pub mod export;
pub mod items;
pub mod locations;
pub mod quick_add;
pub mod search;
pub mod shopping;
pub mod users;

pub fn router(state: AppState) -> Router {
    let photos = ServeDir::new(state.cfg.data_dir.join("photos"));
    Router::new()
        .route("/", get(auth::home))
        .route("/healthz", get(health))
        .route("/login", get(auth::login_page).post(auth::login))
        .route("/setup", post(auth::setup))
        .route("/logout", post(auth::logout))
        .route("/loc", get(locations::root).post(locations::create_root))
        .route("/loc/contents", get(locations::root_contents))
        .route("/loc/new", get(locations::new_root))
        .route(
            "/loc/{id}",
            get(locations::show).post(locations::create_child),
        )
        .route("/loc/{id}/contents", get(locations::contents))
        .route("/loc/{id}/move", post(locations::move_box))
        .route("/loc/{id}/new-box", get(locations::new_child))
        .route("/loc/{id}/move-sheet", get(locations::move_sheet))
        .route("/loc/{id}/edit", get(locations::edit))
        .route("/loc/{id}/rename", post(locations::rename))
        .route("/loc/{id}/delete", post(locations::delete))
        .route(
            "/items/{id}",
            get(items::detail).delete(items::delete).post(items::delete),
        )
        .route("/items/{id}/update", post(items::update))
        .route("/items/{id}/delete", post(items::delete))
        .route("/items/{id}/checkout", post(items::checkout))
        .route("/items/{id}/move", post(items::move_item))
        .route(
            "/items/{id}/consume",
            get(items::consume_dialog).post(items::consume),
        )
        .route("/in-use", get(items::in_use))
        .route("/search", get(search::index))
        .route("/shopping", get(shopping::index).post(shopping::add))
        .route("/shopping/{id}/toggle", post(shopping::toggle))
        .route("/shopping/{id}/remove", post(shopping::remove))
        .route("/shopping/{id}/restock", post(shopping::restock))
        .route("/shopping/from-item/{id}", post(shopping::from_item))
        .route(
            "/shopping/from-item/{id}/remove",
            post(shopping::remove_from_item),
        )
        .route("/shopping/share.txt", get(shopping::share_text_route))
        .route("/shopping/share-sheet", get(shopping::share_sheet))
        .route("/export.csv", get(export::csv))
        .route("/account", get(account::page))
        .route("/account/password", post(account::change_password))
        .route(
            "/account/logout-everywhere",
            post(account::logout_everywhere),
        )
        .route("/me/theme", post(account::theme))
        .route("/household", get(users::page))
        .route("/users", post(users::create))
        .route("/users/{id}/reset", post(users::reset))
        .route("/users/{id}/password", post(users::change_password))
        .route("/users/{id}/role", post(users::role))
        .route("/users/{id}/remove", post(users::remove))
        .route("/checkouts/{id}/return", post(items::return_item))
        .route("/checkouts/{id}/reassign", post(items::reassign))
        .route("/checkouts/{id}/put-away", post(items::put_away))
        .route("/checkouts/{id}/consume", post(items::consume_checkout))
        .route(
            "/loc/{id}/quick-add",
            get(quick_add::sheet)
                .post(quick_add::create)
                .layer(DefaultBodyLimit::max(25 * 1024 * 1024)),
        ) // phone photos
        .route("/quick-add", get(quick_add::last))
        .nest_service("/photos", photos)
        .nest_service("/static", ServeDir::new("static")) // or rust-embed for a single binary
        .layer(middleware::from_fn(crate::error::present))
        .layer(DefaultBodyLimit::max(64 * 1024)) // forms; quick-add raises its own
        .layer(middleware::from_fn(crate::security::same_origin))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::X_FRAME_OPTIONS,
            HeaderValue::from_static("DENY"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static("frame-ancestors 'none'"),
        ))
        .layer(SetResponseHeaderLayer::if_not_present(
            header::REFERRER_POLICY,
            HeaderValue::from_static("same-origin"),
        ))
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn_with_state(
            state.activity.clone(),
            activity::track,
        ))
        .with_state(state)
}

/// HTMX-aware helper: true when the request came from hx-get/hx-post.
pub fn is_htmx(h: &axum::http::HeaderMap) -> bool {
    h.contains_key("HX-Request")
}

/// For the installer and a proxy's health check: 200 once migrations are done and the
/// database answers. Not counted as activity, so frequent checks don't hold off idle backups.
async fn health(
    axum::extract::State(st): axum::extract::State<AppState>,
) -> Result<&'static str, crate::error::AppError> {
    st.db
        .call(|c| Ok(c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0))?))
        .await?;
    Ok("ok")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{activity::Activity, config::Config, db::Db};
    use axum::{body::Body, http::Request};
    use std::{
        fs,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_check_answers_without_counting_as_activity() {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("homestock-health-test-{suffix}"));
        fs::create_dir_all(&dir).expect("create test directory");
        let cfg: Config = toml::from_str(&format!(
            "bind = \"127.0.0.1:0\"\ndata_dir = \"{}\"\n[backup]\n",
            dir.display()
        ))
        .expect("parse config");
        let activity = Arc::new(Activity::default());
        let state = AppState {
            db: Db::open(&cfg.database_path()).expect("open database"),
            cfg: Arc::new(cfg),
            activity: activity.clone(),
            passwords: Default::default(),
        };
        let app = router(state);
        let get = |uri: &str| Request::get(uri).body(Body::empty()).expect("request");

        let response = app.clone().oneshot(get("/healthz")).await.expect("respond");
        assert_eq!(response.status(), axum::http::StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        assert_eq!(&body[..], b"ok");
        assert_eq!(
            activity.last_request(),
            0,
            "health checks must not hold off idle backups"
        );

        app.oneshot(get("/login")).await.expect("respond");
        assert!(
            activity.last_request() > 0,
            "real requests count as activity"
        );
        fs::remove_dir_all(dir).expect("remove test directory");
    }
}
