use crate::{config::SeedUser, error::AppError, AppState};
use argon2::{password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier}, Argon2};
use axum::{extract::{FromRef, FromRequestParts}, http::request::Parts};
use axum_extra::extract::CookieJar;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use rusqlite::{params, Connection, OptionalExtension};

pub const COOKIE: &str = "hs_session";

#[derive(Clone, Debug)]
pub struct CurrentUser { pub id: i64, pub name: String, pub is_admin: bool, pub theme: String, pub must_change_pw: bool }

pub fn hash(pw: &str) -> Result<String, AppError> {
    Argon2::default().hash_password(pw.as_bytes())
        .map(|h| h.to_string()).map_err(|e| AppError::internal(e.to_string()))
}
pub fn verify(pw: &str, stored: &str) -> bool {
    PasswordHash::new(stored).map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok()).unwrap_or(false)
}


/// Only when the user table is empty. Seeded accounts must change their password.
pub fn seed_users(c: &mut Connection, seeds: &[SeedUser]) -> Result<(), AppError> {
    let n: i64 = c.query_row("SELECT COUNT(*) FROM user WHERE deleted_at IS NULL", [], |r| r.get(0))?;
    if n > 0 || seeds.is_empty() { return Ok(()); }
    let tx = c.transaction()?;
    for s in seeds {
        tx.execute("INSERT INTO user(name, role, password_hash, must_change_pw) VALUES (?1, ?2, ?3, 1)",
            params![s.name, s.role, hash(&s.password)?])?;
    }
    tx.commit()?;
    tracing::info!("seeded {} users from config", seeds.len());
    Ok(())
}

/// Valid session for `token`, sliding its expiry to `days` from now.
pub fn lookup_session(c: &Connection, token: &str, days: i64) -> Result<Option<CurrentUser>, AppError> {
    let u = c.query_row(
        "SELECT u.id, u.name, u.role = 'admin', u.theme, u.must_change_pw, s.expires_at
         FROM session s JOIN user u ON u.id = s.user_id
         WHERE s.token = ?1 AND s.expires_at > unixepoch() AND u.deleted_at IS NULL",
        [token], |r| Ok((CurrentUser { id: r.get(0)?, name: r.get(1)?, is_admin: r.get(2)?, theme: r.get(3)?, must_change_pw: r.get(4)? }, r.get::<_, i64>(5)?)),
    ).optional()?;
    if let Some((user, exp)) = u {
        // Sliding expiry, refreshed at most hourly to avoid a write per request.
        c.execute("UPDATE session SET expires_at = unixepoch() + ?3*86400 WHERE token = ?1 AND ?2 < unixepoch() + ?3*86400 - 3600", params![token, exp, days])?;
        return Ok(Some(user));
    }
    Ok(None)
}

impl<S> FromRequestParts<S> for CurrentUser
where AppState: FromRef<S>, S: Send + Sync {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let st = AppState::from_ref(state);
        let jar = CookieJar::from_request_parts(parts, state).await.map_err(|_| AppError::Unauthorized)?;
        let token = jar.get(COOKIE).map(|c| c.value().to_string()).ok_or(AppError::Unauthorized)?;
        let days = st.cfg.session_days;
        let user = st.db.call(move |c| lookup_session(c, &token, days)).await?.ok_or(AppError::Unauthorized)?;
        if user.must_change_pw && !parts.uri.path().starts_with("/account") && parts.uri.path() != "/me/theme" {
            return Err(AppError::PasswordChangeRequired);
        }
        Ok(user)
    }
}

/// Use as an extractor on admin-only handlers.
pub struct AdminUser(pub CurrentUser);
impl<S> FromRequestParts<S> for AdminUser
where AppState: FromRef<S>, S: Send + Sync {
    type Rejection = AppError;
    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let u = CurrentUser::from_request_parts(parts, state).await?;
        if u.is_admin { Ok(AdminUser(u)) } else { Err(AppError::Forbidden) }
    }
}

/// Store a new session for `user_id` (clearing out expired ones) and return its token.
pub fn create_session(c: &Connection, user_id: i64, days: i64) -> Result<String, AppError> {
    let mut token = [0u8; 32];
    rand::fill(&mut token);
    let token = URL_SAFE_NO_PAD.encode(token);
    c.execute("DELETE FROM session WHERE expires_at <= unixepoch()", [])?;
    c.execute("INSERT INTO session(token, user_id, expires_at) VALUES (?1, ?2, unixepoch() + ?3 * 86400)", params![token, user_id, days])?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> Connection {
        let c = Connection::open_in_memory().expect("open in-memory database");
        c.execute_batch(include_str!("../migrations/0001_init.sql")).expect("apply schema");
        c.execute_batch(include_str!("../migrations/0002_soft_delete_users.sql")).expect("apply user migration");
        c.execute("INSERT INTO user(name,role,password_hash) VALUES ('Alex','admin','hash')", []).expect("insert user");
        c
    }

    fn expiry(c: &Connection, token: &str) -> i64 {
        c.query_row("SELECT expires_at - unixepoch() FROM session WHERE token=?1", [token], |r| r.get(0)).expect("read expiry")
    }

    #[test]
    fn sliding_expiry_follows_configured_session_days() {
        let c = database();
        let token = create_session(&c, 1, 90).expect("create session");
        c.execute("UPDATE session SET expires_at = unixepoch() + 86400 WHERE token=?1", [&token]).expect("age session");
        assert!(lookup_session(&c, &token, 90).expect("look up").is_some());
        assert!((90 * 86400 - 5..=90 * 86400).contains(&expiry(&c, &token)));
    }

    #[test]
    fn fresh_sessions_are_not_rewritten_on_every_request() {
        let c = database();
        let token = create_session(&c, 1, 7).expect("create session");
        c.execute("UPDATE session SET expires_at = unixepoch() + 7*86400 - 60 WHERE token=?1", [&token]).expect("touch session");
        lookup_session(&c, &token, 7).expect("look up");
        assert!(expiry(&c, &token) <= 7 * 86400 - 60);
    }

    #[test]
    fn new_sessions_clear_expired_ones() {
        let c = database();
        c.execute("INSERT INTO session(token,user_id,expires_at) VALUES ('old',1,unixepoch()-1)", []).expect("insert expired");
        let token = create_session(&c, 1, 30).expect("create session");
        let tokens: Vec<String> = c.prepare("SELECT token FROM session").expect("prepare")
            .query_map([], |r| r.get(0)).expect("query").collect::<Result<_, _>>().expect("collect");
        assert_eq!(tokens, vec![token]);
    }
}
