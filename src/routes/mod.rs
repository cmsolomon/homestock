use crate::{activity, AppState};
use axum::{extract::DefaultBodyLimit, middleware, routing::{get, post}, Router};
use tower_http::{compression::CompressionLayer, services::ServeDir};

pub mod locations;
pub mod quick_add;
pub mod auth;
pub mod items;
pub mod search;
pub mod shopping;
pub mod export;
pub mod account;
pub mod users;
// pub mod items; pub mod checkouts; pub mod shopping; pub mod search; pub mod users; pub mod account; pub mod export;

pub fn router(state: AppState) -> Router {
    let photos = ServeDir::new(state.cfg.data_dir.join("photos"));
    Router::new()
        .route("/", get(auth::home))
        .route("/login", get(auth::login_page).post(auth::login))
        .route("/setup", post(auth::setup))
        .route("/logout", post(auth::logout))
        .route("/loc", get(locations::root).post(locations::create_root))
        .route("/loc/contents", get(locations::root_contents))
        .route("/loc/new", get(locations::new_root))
        .route("/loc/{id}", get(locations::show).post(locations::create_child))
        .route("/loc/{id}/contents", get(locations::contents))
        .route("/loc/{id}/move", post(locations::move_box))
        .route("/loc/{id}/new-box", get(locations::new_child))
        .route("/loc/{id}/move-sheet", get(locations::move_sheet))
        .route("/loc/{id}/edit", get(locations::edit))
        .route("/loc/{id}/rename", post(locations::rename))
        .route("/loc/{id}/delete", post(locations::delete))
        .route("/items/{id}", get(items::detail).delete(items::delete).post(items::delete))
        .route("/items/{id}/update", post(items::update))
        .route("/items/{id}/delete", post(items::delete))
        .route("/items/{id}/checkout", post(items::checkout))
        .route("/items/{id}/move", post(items::move_item))
        .route("/items/{id}/consume", get(items::consume_dialog).post(items::consume))
        .route("/in-use", get(items::in_use))
        .route("/search", get(search::index))
        .route("/shopping", get(shopping::index).post(shopping::add))
        .route("/shopping/{id}/toggle", post(shopping::toggle))
        .route("/shopping/{id}/remove", post(shopping::remove))
        .route("/shopping/{id}/restock", post(shopping::restock))
        .route("/shopping/from-item/{id}", post(shopping::from_item))
        .route("/shopping/from-item/{id}/remove", post(shopping::remove_from_item))
        .route("/shopping/share.txt", get(shopping::share_text_route))
        .route("/shopping/share-sheet", get(shopping::share_sheet))
        .route("/export.csv", get(export::csv))
        .route("/account", get(account::page))
        .route("/account/password", post(account::change_password))
        .route("/account/logout-everywhere", post(account::logout_everywhere))
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
