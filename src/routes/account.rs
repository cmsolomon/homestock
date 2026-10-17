use crate::{auth::{self, CurrentUser}, error::AppError, AppState};
use askama::Template;
use axum::{extract::{Form, State}, response::{Html, Redirect}};
use rusqlite::params;
use serde::Deserialize;

#[derive(Template)]
#[template(path = "account.html")]
pub struct AccountPage { pub user: CurrentUser }

pub async fn page(user: CurrentUser) -> Result<Html<String>, AppError> {
    Ok(Html(AccountPage { user }.render()?))
}

#[derive(Deserialize)]
pub struct PasswordForm { current: String, next: String, confirm: String }

pub async fn change_password(State(st): State<AppState>, user: CurrentUser, Form(f): Form<PasswordForm>) -> Result<Redirect, AppError> {
    if f.next.len() < 8 { return Err(AppError::bad("New password needs at least 8 characters.")); }
    if f.next != f.confirm { return Err(AppError::bad("The two new passwords don’t match.")); }
    let user_id = user.id;
    let old_hash = st.db.call(move |c| {
        c.query_row("SELECT password_hash FROM user WHERE id=?1",[user_id],|r|r.get::<_,String>(0)).map_err(Into::into)
    }).await?;
    let current = f.current;
    let hash_copy=old_hash.clone();
    let valid=tokio::task::spawn_blocking(move ||auth::verify(&current,&hash_copy))
        .await.map_err(|e|AppError::internal(e.to_string()))?;
    if !valid { return Err(AppError::bad("Current password is incorrect.")); }
    let next=f.next;
    let old_hash_for_check=old_hash.clone();
    let password_hash=tokio::task::spawn_blocking(move || {
        if auth::verify(&next,&old_hash_for_check) {
            return Err(AppError::bad("Choose a password different from the current one."));
        }
        auth::hash(&next)
    }).await.map_err(|e|AppError::internal(e.to_string()))??;
    st.db.call(move |c| {
        c.execute("UPDATE user SET password_hash=?2,must_change_pw=0 WHERE id=?1",params![user_id,password_hash])?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/account"))
}

#[derive(Deserialize)]
pub struct ThemeForm { theme: String }

pub async fn theme(State(st): State<AppState>, user: CurrentUser, Form(f): Form<ThemeForm>) -> Result<Redirect, AppError> {
    if !["light","dark","system"].contains(&f.theme.as_str()) { return Err(AppError::bad("Choose a valid theme.")); }
    st.db.call(move |c| {
        c.execute("UPDATE user SET theme=?2 WHERE id=?1",params![user.id,f.theme])?;
        Ok(())
    }).await?;
    Ok(Redirect::to("/account"))
}
