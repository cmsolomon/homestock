use crate::{auth::{self, COOKIE}, error::{AppError, TOO_MANY_ATTEMPTS}, AppState};
use askama::Template;
use axum::{
    extract::{ConnectInfo, Form, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::CookieJar;
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;
use std::net::SocketAddr;

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    names: Vec<String>,
    first_run: bool,
    error: Option<String>,
}

async fn page(st: &AppState, error: Option<String>) -> Result<Html<String>, AppError> {
    let names = st.db.call(|c| {
        let mut stmt = c.prepare_cached("SELECT name FROM user WHERE deleted_at IS NULL ORDER BY name COLLATE NOCASE")?;
        let names = stmt.query_map([], |row| row.get(0))?.collect::<Result<Vec<String>, _>>()?;
        Ok(names)
    }).await?;
    let first_run = names.is_empty();
    Ok(Html(LoginPage { names, first_run, error }.render()?))
}

pub async fn login_page(State(st): State<AppState>) -> Result<Html<String>, AppError> {
    page(&st, None).await
}

pub async fn home(State(st): State<AppState>, jar: CookieJar) -> Result<Response, AppError> {
    if let Some(cookie) = jar.get(COOKIE) {
        let token = cookie.value().to_owned();
        let days = st.cfg.session_days;
        if let Some(user) = st.db.call(move |c| auth::lookup_session(c, &token, days)).await? {
            return Ok(Redirect::to(if user.must_change_pw { "/account" } else { "/loc" }).into_response());
        }
    }
    Ok(page(&st, None).await?.into_response())
}

#[derive(Deserialize)]
pub struct Credentials {
    name: String,
    password: String,
}

const WRONG_PASSWORD: &str = "That password doesn’t match. Try again or ask an admin to reset it.";

pub async fn login(State(st): State<AppState>, ConnectInfo(peer): ConnectInfo<SocketAddr>, Form(f): Form<Credentials>) -> Result<Response, AppError> {
    let ip = peer.ip();
    if !st.passwords.allowed(ip) {
        return Ok((StatusCode::TOO_MANY_REQUESTS, page(&st, Some(TOO_MANY_ATTEMPTS.into())).await?).into_response());
    }
    let name = f.name.trim().to_string();
    let password = f.password;
    let user = if password.len() > auth::MAX_PASSWORD_LEN { None } else {
        st.db.call(move |c| {
            c.query_row(
                "SELECT id, password_hash FROM user WHERE name = ?1 COLLATE NOCASE AND deleted_at IS NULL",
                [&name],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            ).optional().map_err(Into::into)
        }).await?
    };

    let valid = match user {
        Some((user_id, hash)) => tokio::task::spawn_blocking(move || auth::verify(&password, &hash).then_some(user_id))
            .await.map_err(|e| AppError::internal(e.to_string()))?,
        None => None,
    };
    let Some(user_id) = valid else {
        st.passwords.failed(ip);
        return Ok(page(&st, Some(WRONG_PASSWORD.into())).await?.into_response());
    };
    st.passwords.succeeded(ip);

    let days = st.cfg.session_days;
    let (token, must_change) = st.db.call(move |c| {
        let token = auth::create_session(c, user_id, days)?;
        let must_change = c.query_row("SELECT must_change_pw FROM user WHERE id = ?1 AND deleted_at IS NULL", [user_id], |row| row.get::<_, bool>(0))?;
        Ok((token, must_change))
    }).await?;
    let mut response = Redirect::to(if must_change { "/account" } else { "/loc" }).into_response();
    let cookie = session_cookie(&token, st.cfg.cookie_secure);
    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).map_err(|e| AppError::internal(e.to_string()))?);
    Ok(response)
}

#[derive(Deserialize)]
pub struct SetupForm {
    name: String,
    password: String,
    confirm: String,
}

pub async fn setup(State(st): State<AppState>, Form(f): Form<SetupForm>) -> Result<Response, AppError> {
    if f.name.trim().is_empty() || f.name.trim().len() > 60 {
        return Ok(page(&st, Some("Enter a name (up to 60 characters).".into())).await?.into_response());
    }
    if let Err(error) = auth::validate_new_password(&f.password, &f.confirm) {
        return Ok(page(&st, Some(error.to_string())).await?.into_response());
    }
    // Check before hashing so this unauthenticated endpoint can't be used to burn CPU on argon2.
    let users: i64 = st.db.call(|c| Ok(c.query_row("SELECT COUNT(*) FROM user WHERE deleted_at IS NULL", [], |row| row.get(0))?)).await?;
    if users != 0 { return Err(AppError::conflict("Household setup is already complete")); }
    let password = f.password;
    let password_hash = tokio::task::spawn_blocking(move || auth::hash(&password))
        .await.map_err(|e| AppError::internal(e.to_string()))??;
    let name = f.name.trim().to_owned();
    let user_id = st.db.call(move |c| {
        let tx = c.transaction()?;
        let count: i64 = tx.query_row("SELECT COUNT(*) FROM user WHERE deleted_at IS NULL", [], |row| row.get(0))?;
        if count != 0 { return Err(AppError::conflict("Household setup is already complete")); }
        crate::routes::users::ensure_name_available(&tx, &name)?;
        tx.execute("INSERT INTO user(name, role, password_hash) VALUES (?1, 'admin', ?2)", params![name, password_hash])?;
        let id = tx.last_insert_rowid();
        tx.commit()?;
        Ok(id)
    }).await?;
    let mut response = Redirect::to("/loc").into_response();
    let days = st.cfg.session_days;
    let token = st.db.call(move |c| auth::create_session(c, user_id, days)).await?;
    let cookie = session_cookie(&token, st.cfg.cookie_secure);
    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).map_err(|e| AppError::internal(e.to_string()))?);
    Ok(response)
}

pub async fn logout(State(st): State<AppState>, headers: HeaderMap) -> Result<Response, AppError> {
    let token = headers.get(header::COOKIE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').find_map(|p| p.trim().strip_prefix(&format!("{COOKIE}=")))
            .map(str::to_string));
    if let Some(token) = token {
        st.db.call(move |c| {
            c.execute("DELETE FROM session WHERE token = ?1", [&token])?;
            Ok(())
        }).await?;
    }
    let mut response = Redirect::to("/login").into_response();
    let secure = if st.cfg.cookie_secure { "; Secure" } else { "" };
    let cookie = format!("{COOKIE}=; HttpOnly; SameSite=Lax{secure}; Path=/; Max-Age=0");
    response.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).map_err(|e| AppError::internal(e.to_string()))?);
    Ok(response)
}

/// The server-side sliding expiry (`session_days`) decides how long a session lives; the
/// cookie only carries the token, so it uses the longest lifetime browsers accept (400 days)
/// and an active user is never signed out by a cookie set once at login.
fn session_cookie(token: &str, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE}={token}; HttpOnly; SameSite=Lax{secure}; Path=/; Max-Age={}", 400 * 86400)
}

#[cfg(test)]
mod tests {
    use super::session_cookie;

    #[test]
    fn secure_cookie_flag_follows_config() {
        assert!(session_cookie("token",true).contains("; Secure;"));
        assert!(!session_cookie("token",false).contains("; Secure;"));
    }
}
