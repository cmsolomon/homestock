use crate::{
    auth::{self, CurrentUser, COOKIE},
    error::AppError,
    security::ClientIp,
    AppState,
};
use askama::Template;
use axum::{
    extract::{Form, State},
    http::{header, HeaderValue},
    response::{Html, IntoResponse, Redirect, Response},
};
use axum_extra::extract::CookieJar;
use rusqlite::params;
use serde::Deserialize;

#[derive(Template)]
#[template(path = "account.html")]
pub struct AccountPage {
    pub user: CurrentUser,
}

pub async fn page(user: CurrentUser) -> Result<Html<String>, AppError> {
    Ok(Html(AccountPage { user }.render()?))
}

pub async fn logout_everywhere(
    State(st): State<AppState>,
    user: CurrentUser,
) -> Result<Response, AppError> {
    let user_id = user.id;
    st.db.call(move |c| revoke_sessions(c, user_id)).await?;
    let secure = if st.cfg.cookie_secure { "; Secure" } else { "" };
    let cookie = format!("{COOKIE}=; HttpOnly; SameSite=Lax{secure}; Path=/; Max-Age=0");
    let mut response = Redirect::to("/login").into_response();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_str(&cookie).map_err(|e| AppError::internal(e.to_string()))?,
    );
    Ok(response)
}

fn revoke_sessions(c: &rusqlite::Connection, user_id: i64) -> Result<(), AppError> {
    c.execute("DELETE FROM session WHERE user_id=?1", [user_id])?;
    Ok(())
}

#[derive(Deserialize)]
pub struct PasswordForm {
    current: String,
    next: String,
    confirm: String,
}

/// Also signs out every other session of this user, keeping the one making the change.
pub async fn change_password(
    State(st): State<AppState>,
    user: CurrentUser,
    ClientIp(ip): ClientIp,
    jar: CookieJar,
    Form(f): Form<PasswordForm>,
) -> Result<Redirect, AppError> {
    auth::validate_new_password(&f.next, &f.confirm)?;
    if !st.passwords.allowed(ip) {
        return Err(AppError::TooManyAttempts);
    }
    if f.current.len() > auth::MAX_PASSWORD_LEN {
        st.passwords.failed(ip);
        return Err(AppError::bad("Current password is incorrect."));
    }
    let user_id = user.id;
    let old_hash = st
        .db
        .call(move |c| {
            c.query_row(
                "SELECT password_hash FROM user WHERE id=?1",
                [user_id],
                |r| r.get::<_, String>(0),
            )
            .map_err(Into::into)
        })
        .await?;
    let current = f.current;
    let hash_copy = old_hash.clone();
    let valid = tokio::task::spawn_blocking(move || auth::verify(&current, &hash_copy))
        .await
        .map_err(|e| AppError::internal(e.to_string()))?;
    if !valid {
        st.passwords.failed(ip);
        return Err(AppError::bad("Current password is incorrect."));
    }
    st.passwords.succeeded(ip);
    let next = f.next;
    let password_hash = tokio::task::spawn_blocking(move || {
        if auth::verify(&next, &old_hash) {
            return Err(AppError::bad(
                "Choose a password different from the current one.",
            ));
        }
        auth::hash(&next)
    })
    .await
    .map_err(|e| AppError::internal(e.to_string()))??;
    let keep = jar
        .get(COOKIE)
        .map(|c| c.value().to_owned())
        .unwrap_or_default();
    st.db
        .call(move |c| set_own_password(c, user_id, &password_hash, &keep))
        .await?;
    Ok(Redirect::to("/account"))
}

fn set_own_password(
    c: &mut rusqlite::Connection,
    user_id: i64,
    password_hash: &str,
    keep_token: &str,
) -> Result<(), AppError> {
    let tx = c.transaction()?;
    tx.execute(
        "UPDATE user SET password_hash=?2,must_change_pw=0 WHERE id=?1",
        params![user_id, password_hash],
    )?;
    tx.execute(
        "DELETE FROM session WHERE user_id=?1 AND token<>?2",
        params![user_id, keep_token],
    )?;
    tx.commit()?;
    Ok(())
}

#[derive(Deserialize)]
pub struct ThemeForm {
    theme: String,
    return_to: Option<String>,
}

pub async fn theme(
    State(st): State<AppState>,
    user: CurrentUser,
    Form(f): Form<ThemeForm>,
) -> Result<Redirect, AppError> {
    if !["light", "dark", "system"].contains(&f.theme.as_str()) {
        return Err(AppError::bad("Choose a valid theme."));
    }
    let return_to = f.return_to.as_deref().unwrap_or("/account");
    if !["/", "/account"].contains(&return_to) {
        return Err(AppError::bad("Invalid return destination."));
    }
    st.db
        .call(move |c| {
            c.execute(
                "UPDATE user SET theme=?2 WHERE id=?1",
                params![user.id, f.theme],
            )?;
            Ok(())
        })
        .await?;
    Ok(Redirect::to(return_to))
}

#[cfg(test)]
mod tests {
    use super::{revoke_sessions, set_own_password};
    use rusqlite::Connection;

    #[test]
    fn logging_out_everywhere_removes_only_the_users_sessions() {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql"))
            .expect("apply schema");
        c.execute_batch(include_str!("../../migrations/0002_soft_delete_users.sql"))
            .expect("apply user migration");
        c.execute_batch(
            "INSERT INTO user(name,role,password_hash) VALUES ('Alex','admin','hash'),('Sam','member','hash');
             INSERT INTO session(token,user_id,expires_at) VALUES
               ('alex-device-1',1,9999999999),('alex-device-2',1,9999999999),('sam-device',2,9999999999);",
        ).expect("insert users and sessions");

        revoke_sessions(&c, 1).expect("revoke Alex sessions");

        let alex_sessions: i64 = c
            .query_row("SELECT COUNT(*) FROM session WHERE user_id=1", [], |r| {
                r.get(0)
            })
            .expect("count Alex sessions");
        let sam_sessions: i64 = c
            .query_row("SELECT COUNT(*) FROM session WHERE user_id=2", [], |r| {
                r.get(0)
            })
            .expect("count Sam sessions");
        assert_eq!(alex_sessions, 0);
        assert_eq!(sam_sessions, 1);
    }

    #[test]
    fn changing_own_password_keeps_only_the_current_session() {
        let mut c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../../migrations/0001_init.sql"))
            .expect("apply schema");
        c.execute_batch(include_str!("../../migrations/0002_soft_delete_users.sql"))
            .expect("apply user migration");
        c.execute_batch(
            "INSERT INTO user(name,role,password_hash,must_change_pw) VALUES ('Alex','admin','old',1),('Sam','member','hash',0);
             INSERT INTO session(token,user_id,expires_at) VALUES
               ('this-device',1,9999999999),('lost-phone',1,9999999999),('sam-device',2,9999999999);",
        ).expect("insert users and sessions");

        set_own_password(&mut c, 1, "new", "this-device").expect("change password");

        let tokens: Vec<String> = c
            .prepare("SELECT token FROM session ORDER BY token")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<Result<_, _>>()
            .expect("collect");
        assert_eq!(tokens, ["sam-device", "this-device"]);
        let (hash, must_change): (String, bool) = c
            .query_row(
                "SELECT password_hash,must_change_pw FROM user WHERE id=1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("read user");
        assert_eq!((hash.as_str(), must_change), ("new", false));
    }
}
