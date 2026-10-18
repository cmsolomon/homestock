use crate::{auth::{self, COOKIE}, error::AppError, AppState};
use askama::Template;
use axum::{
    extract::{Form, State},
    http::{header, HeaderMap, HeaderValue},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::CookieJar;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, OptionalExtension};
use serde::Deserialize;

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
        if let Some(user) = st.db.call(move |c| auth::lookup_session(c, &token)).await? {
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

pub async fn login(State(st): State<AppState>, Form(f): Form<Credentials>) -> Result<Response, AppError> {
    let name = f.name.trim().to_string();
    let password = f.password;
    let user = st.db.call(move |c| {
        c.query_row(
            "SELECT id, password_hash FROM user WHERE name = ?1 COLLATE NOCASE AND deleted_at IS NULL",
            [&name],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        ).optional().map_err(Into::into)
    }).await?;

    let Some((user_id, hash)) = user else {
        return Ok(page(&st, Some("That password doesn’t match. Try again or ask an admin to reset it.".into())).await?.into_response());
    };
    let valid = tokio::task::spawn_blocking(move || auth::verify(&password, &hash))
        .await.map_err(|e| AppError::internal(e.to_string()))?;
    if !valid {
        return Ok(page(&st, Some("That password doesn’t match. Try again or ask an admin to reset it.".into())).await?.into_response());
    }

    let mut token = [0u8; 32];
    rand::fill(&mut token);
    let token = URL_SAFE_NO_PAD.encode(token);
    let token_for_db = token.clone();
    let days = st.cfg.session_days;
    let must_change = st.db.call(move |c| {
        c.execute(
            "INSERT INTO session(token, user_id, expires_at) VALUES (?1, ?2, unixepoch() + ?3 * 86400)",
            params![token_for_db, user_id, days],
        )?;
        c.query_row("SELECT must_change_pw FROM user WHERE id = ?1 AND deleted_at IS NULL", [user_id], |row| row.get::<_, bool>(0)).map_err(Into::into)
    }).await?;
    let mut response = Redirect::to(if must_change { "/account" } else { "/loc" }).into_response();
    let cookie = session_cookie(&token, days, st.cfg.cookie_secure);
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
    if f.password.len() < 8 {
        return Ok(page(&st, Some("Password needs at least 8 characters.".into())).await?.into_response());
    }
    if f.password != f.confirm {
        return Ok(page(&st, Some("The two passwords don’t match.".into())).await?.into_response());
    }
    let password_hash = auth::hash(&f.password)?;
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
    let mut token = [0u8; 32];
    rand::fill(&mut token);
    let token = URL_SAFE_NO_PAD.encode(token);
    let token_db = token.clone();
    let days = st.cfg.session_days;
    st.db.call(move |c| {
        c.execute("INSERT INTO session(token, user_id, expires_at) VALUES (?1, ?2, unixepoch() + ?3 * 86400)", params![token_db, user_id, days])?;
        Ok(())
    }).await?;
    let cookie = session_cookie(&token, days, st.cfg.cookie_secure);
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

fn session_cookie(token: &str, days: i64, secure: bool) -> String {
    let secure = if secure { "; Secure" } else { "" };
    format!("{COOKIE}={token}; HttpOnly; SameSite=Lax{secure}; Path=/; Max-Age={}", days * 86400)
}

#[cfg(test)]
mod tests {
    use super::session_cookie;

    #[test]
    fn secure_cookie_flag_follows_config() {
        assert!(session_cookie("token",30,true).contains("; Secure;"));
        assert!(!session_cookie("token",30,false).contains("; Secure;"));
    }
}
