use crate::{activity, AppState};
use axum::{extract::DefaultBodyLimit, middleware, response::Redirect, routing::{get, post}, Router};
use tower_http::{compression::CompressionLayer, services::ServeDir};

pub mod locations;
pub mod quick_add;
// pub mod items; pub mod checkouts; pub mod shopping; pub mod search; pub mod users; pub mod account; pub mod export;

pub fn router(state: AppState) -> Router {
    let photos = ServeDir::new(state.cfg.data_dir.join("photos"));
    Router::new()
        .route("/", get(|| async { Redirect::to("/loc") }))
        // .route("/login", get(auth::login_page).post(auth::login))  .route("/logout", post(auth::logout))
        .route("/loc", get(locations::root))
        .route("/loc/{id}", get(locations::show))
        .route("/loc/{id}/contents", get(locations::contents))
        .route("/loc/{id}/move", post(locations::move_box))
        .route("/loc/{id}/quick-add", get(quick_add::sheet).post(quick_add::create))
        .route("/quick-add", get(quick_add::last))
        .nest_service("/photos", photos)
        .nest_service("/static", ServeDir::new("static"))   // or rust-embed for a single binary
        .layer(DefaultBodyLimit::max(25 * 1024 * 1024))     // phone photos
        .layer(CompressionLayer::new())
        .layer(middleware::from_fn_with_state(state.activity.clone(), activity::track))
        .with_state(state)
}

/// HTMX-aware helper: true when the request came from hx-get/hx-post.
pub fn is_htmx(h: &axum::http::HeaderMap) -> bool { h.contains_key("HX-Request") }
