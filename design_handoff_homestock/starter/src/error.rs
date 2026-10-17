use axum::{http::{HeaderMap, HeaderValue, StatusCode}, response::{Html, IntoResponse, Response}};

#[derive(Debug)]
pub enum AppError {
    NotFound,
    BadRequest(String),
    Conflict(String),
    Unauthorized,
    Forbidden,
    Internal(String),
}

impl AppError {
    pub fn internal(m: impl Into<String>) -> Self { AppError::Internal(m.into()) }
    pub fn bad(m: impl Into<String>) -> Self { AppError::BadRequest(m.into()) }
    pub fn conflict(m: impl Into<String>) -> Self { AppError::Conflict(m.into()) }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::QueryReturnedNoRows => AppError::NotFound,
            rusqlite::Error::SqliteFailure(_, Some(ref msg)) if msg.contains("exceeds available")
                || msg.contains("below checked-out") => AppError::Conflict(msg.clone()),
            other => AppError::Internal(other.to_string()),
        }
    }
}
impl From<askama::Error> for AppError { fn from(e: askama::Error) -> Self { AppError::Internal(e.to_string()) } }
impl From<std::io::Error> for AppError { fn from(e: std::io::Error) -> Self { AppError::Internal(e.to_string()) } }

/// Errors render as a toast. On HTMX requests the response is retargeted to #toast
/// so the page stays put; on full requests a minimal page is returned.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found".to_string()),
            AppError::BadRequest(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AppError::Unauthorized => {
                let mut h = HeaderMap::new();
                h.insert("HX-Redirect", HeaderValue::from_static("/login"));
                h.insert("Location", HeaderValue::from_static("/login"));
                return (StatusCode::SEE_OTHER, h).into_response();
            }
            AppError::Forbidden => (StatusCode::FORBIDDEN, "Only admins can do that".to_string()),
            AppError::Internal(m) => { tracing::error!(%m); (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong — try again".to_string()) }
        };
        let mut h = HeaderMap::new();
        h.insert("HX-Retarget", HeaderValue::from_static("#toast"));
        h.insert("HX-Reswap", HeaderValue::from_static("innerHTML"));
        let body = format!(r#"<div role="alert" class="toast toast-error">{}</div>"#, html_escape(&msg));
        (status, h, Html(body)).into_response()
    }
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}
