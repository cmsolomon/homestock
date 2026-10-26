use askama::Template;
use axum::{
    extract::Request,
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    middleware::Next,
    response::{Html, IntoResponse, Response},
};
use std::fmt;

#[derive(Debug)]
pub enum AppError {
    NotFound,
    BadRequest(String),
    Conflict(String),
    Unauthorized,
    PasswordChangeRequired,
    Forbidden,
    TooManyAttempts,
    Internal(String),
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("Not found"),
            Self::BadRequest(message) | Self::Conflict(message) | Self::Internal(message) => {
                f.write_str(message)
            }
            Self::Unauthorized => f.write_str("Sign in required"),
            Self::PasswordChangeRequired => f.write_str("Change your password before continuing"),
            Self::Forbidden => f.write_str("Only admins can do that"),
            Self::TooManyAttempts => f.write_str(TOO_MANY_ATTEMPTS),
        }
    }
}

impl std::error::Error for AppError {}

pub const TOO_MANY_ATTEMPTS: &str = "Too many wrong passwords. Wait a minute and try again.";

impl AppError {
    pub fn internal(m: impl Into<String>) -> Self {
        AppError::Internal(m.into())
    }
    pub fn bad(m: impl Into<String>) -> Self {
        AppError::BadRequest(m.into())
    }
    pub fn conflict(m: impl Into<String>) -> Self {
        AppError::Conflict(m.into())
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        match e {
            rusqlite::Error::QueryReturnedNoRows => AppError::NotFound,
            rusqlite::Error::SqliteFailure(_, Some(ref msg))
                if msg.contains("exceeds available") || msg.contains("below checked-out") =>
            {
                AppError::Conflict(msg.clone())
            }
            // A form referred to a location, item or person that was removed meanwhile.
            rusqlite::Error::SqliteFailure(ref err, _)
                if err.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY =>
            {
                AppError::conflict("That changed in the meantime — refresh and try again")
            }
            other => AppError::Internal(other.to_string()),
        }
    }
}
impl From<askama::Error> for AppError {
    fn from(e: askama::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}
impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        AppError::Internal(e.to_string())
    }
}

/// Attached to every error response so `present` can shape it for the request that caused it.
#[derive(Clone)]
enum Presentation {
    Redirect(&'static str),
    Toast(String),
}

/// Errors render as a toast retargeted to #toast (the HTMX shape); `present` adapts that
/// for redirects and non-HTMX requests.
impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let (status, msg) = match &self {
            AppError::NotFound => (StatusCode::NOT_FOUND, "Not found".to_string()),
            AppError::BadRequest(m) => (StatusCode::UNPROCESSABLE_ENTITY, m.clone()),
            AppError::Conflict(m) => (StatusCode::CONFLICT, m.clone()),
            AppError::Unauthorized => return redirect("/login"),
            AppError::PasswordChangeRequired => return redirect("/account"),
            AppError::Forbidden => (StatusCode::FORBIDDEN, "Only admins can do that".to_string()),
            AppError::TooManyAttempts => {
                (StatusCode::TOO_MANY_REQUESTS, TOO_MANY_ATTEMPTS.to_string())
            }
            AppError::Internal(m) => {
                tracing::error!(%m);
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Something went wrong — try again".to_string(),
                )
            }
        };
        let mut h = HeaderMap::new();
        h.insert("HX-Retarget", HeaderValue::from_static("#toast"));
        h.insert("HX-Reswap", HeaderValue::from_static("innerHTML"));
        let body = format!(
            r#"<div role="alert" class="toast toast-error">{}</div>"#,
            html_escape(&msg)
        );
        let mut response = (status, h, Html(body)).into_response();
        response.extensions_mut().insert(Presentation::Toast(msg));
        response
    }
}

fn redirect(to: &'static str) -> Response {
    let mut response = (StatusCode::SEE_OTHER, [(header::LOCATION, to)]).into_response();
    response.extensions_mut().insert(Presentation::Redirect(to));
    response
}

/// Router middleware that shapes `AppError` responses for the request:
/// - HTMX + redirect: 200 with `HX-Redirect`, because the browser would follow a 303
///   itself and htmx would swap the destination page into the target.
/// - HTMX + toast: unchanged (app.js lets htmx swap error statuses into #toast).
/// - Form POST + toast: 303 back to the referring page with a one-shot flash cookie.
/// - Page GET + toast: a standalone error page.
pub async fn present(req: Request, next: Next) -> Response {
    let htmx = crate::routes::is_htmx(req.headers());
    let navigation = req.method() == Method::GET || req.method() == Method::HEAD;
    let back = back_path(req.headers());
    let mut response = next.run(req).await;
    let Some(presentation) = response.extensions_mut().remove::<Presentation>() else {
        return response;
    };
    match presentation {
        Presentation::Redirect(to) if htmx => {
            (StatusCode::OK, [("HX-Redirect", to)]).into_response()
        }
        Presentation::Toast(msg) if !htmx && !navigation => {
            let cookie = format!(
                "{FLASH_COOKIE}={}; Path=/; Max-Age=60; SameSite=Lax",
                percent_encode(&msg)
            );
            let mut redirect = (StatusCode::SEE_OTHER, [(header::LOCATION, back)]).into_response();
            if let Ok(value) = HeaderValue::from_str(&cookie) {
                redirect.headers_mut().insert(header::SET_COOKIE, value);
            }
            redirect
        }
        Presentation::Toast(msg) if !htmx => {
            let page = ErrorPage { message: msg }
                .render()
                .unwrap_or_else(|_| "Something went wrong".into());
            (response.status(), Html(page)).into_response()
        }
        _ => response,
    }
}

/// Read by app.js on page load and shown as an error toast.
const FLASH_COOKIE: &str = "hs_flash";

#[derive(Template)]
#[template(path = "error.html")]
struct ErrorPage {
    message: String,
}

/// Path and query of the Referer, so the redirect always stays on this origin.
fn back_path(h: &HeaderMap) -> String {
    h.get(header::REFERER)
        .and_then(|v| v.to_str().ok())
        .and_then(|r| r.split_once("://"))
        .map(|(_, rest)| rest)
        .and_then(|rest| rest.find('/').map(|i| &rest[i..]))
        .filter(|path| !path.starts_with("//"))
        .unwrap_or("/")
        .to_owned()
}

/// `encodeURIComponent`-compatible, so app.js can `decodeURIComponent` the flash cookie.
fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z'
            | b'a'..=b'z'
            | b'0'..=b'9'
            | b'-'
            | b'_'
            | b'.'
            | b'!'
            | b'~'
            | b'*'
            | b'\''
            | b'('
            | b')' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        routing::{get, post},
        Router,
    };
    use tower::ServiceExt;

    fn app() -> Router {
        Router::new()
            .route(
                "/conflict",
                post(|| async { Err::<(), _>(AppError::conflict("Only 3 available to move")) })
                    .get(|| async { Err::<(), _>(AppError::NotFound) }),
            )
            .route(
                "/private",
                get(|| async { Err::<(), _>(AppError::Unauthorized) }),
            )
            .layer(axum::middleware::from_fn(present))
    }

    fn request(method: Method, uri: &str, htmx: bool) -> Request {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::REFERER, "https://homestock.lan:8443/loc/7?item=3");
        if htmx {
            builder = builder.header("HX-Request", "true");
        }
        builder.body(Body::empty()).expect("build request")
    }

    async fn body(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read body");
        String::from_utf8(bytes.to_vec()).expect("utf-8 body")
    }

    #[tokio::test]
    async fn htmx_auth_failure_uses_hx_redirect_instead_of_a_followed_303() {
        let response = app()
            .oneshot(request(Method::GET, "/private", true))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()["HX-Redirect"], "/login");
        assert!(response.headers().get(header::LOCATION).is_none());
    }

    #[tokio::test]
    async fn page_auth_failure_redirects_to_login() {
        let response = app()
            .oneshot(request(Method::GET, "/private", false))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/login");
    }

    #[tokio::test]
    async fn htmx_error_keeps_the_toast_retarget() {
        let response = app()
            .oneshot(request(Method::POST, "/conflict", true))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::CONFLICT);
        assert_eq!(response.headers()["HX-Retarget"], "#toast");
        assert!(body(response).await.contains("Only 3 available to move"));
    }

    #[tokio::test]
    async fn form_post_error_returns_to_the_referring_page_with_a_flash() {
        let response = app()
            .oneshot(request(Method::POST, "/conflict", false))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::SEE_OTHER);
        assert_eq!(response.headers()[header::LOCATION], "/loc/7?item=3");
        let cookie = response.headers()[header::SET_COOKIE]
            .to_str()
            .expect("ascii cookie");
        assert!(cookie.starts_with("hs_flash=Only%203%20available%20to%20move;"));
    }

    #[tokio::test]
    async fn page_error_renders_a_full_page() {
        let response = app()
            .oneshot(request(Method::GET, "/conflict", false))
            .await
            .expect("respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let html = body(response).await;
        assert!(html.starts_with("<!doctype html>") && html.contains("Not found"));
    }

    #[test]
    fn foreign_key_violations_become_a_refresh_conflict() {
        let c = rusqlite::Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch("PRAGMA foreign_keys=ON; CREATE TABLE p(id INTEGER PRIMARY KEY); CREATE TABLE ch(p INTEGER REFERENCES p(id));").expect("schema");
        let error: AppError = c
            .execute("INSERT INTO ch VALUES (99)", [])
            .expect_err("missing parent")
            .into();
        assert!(matches!(error, AppError::Conflict(m) if m.contains("refresh")));
    }

    #[test]
    fn back_path_never_leaves_this_origin() {
        let referer = |value: &str| {
            let mut h = HeaderMap::new();
            h.insert(
                header::REFERER,
                HeaderValue::from_str(value).expect("header"),
            );
            back_path(&h)
        };
        assert_eq!(referer("https://evil.example/loc/1"), "/loc/1");
        assert_eq!(referer("https://host//evil.example"), "/");
        assert_eq!(referer("https://host"), "/");
        assert_eq!(back_path(&HeaderMap::new()), "/");
    }

    #[test]
    fn flash_encoding_matches_decode_uri_component() {
        assert_eq!(
            percent_encode("It’s already there"),
            "It%E2%80%99s%20already%20there"
        );
    }
}
