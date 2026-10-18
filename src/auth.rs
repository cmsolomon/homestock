use crate::{config::SeedUser, error::AppError, AppState};
use argon2::{password_hash::{phc::PasswordHash, PasswordHasher, PasswordVerifier}, Argon2};
use axum::{extract::{FromRef, FromRequestParts}, http::request::Parts};
use axum_extra::extract::CookieJar;
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

pub fn lookup_session(c: &Connection, token: &str) -> Result<Option<CurrentUser>, AppError> {
    let u = c.query_row(
        "SELECT u.id, u.name, u.role = 'admin', u.theme, u.must_change_pw, s.expires_at
         FROM session s JOIN user u ON u.id = s.user_id
         WHERE s.token = ?1 AND s.expires_at > unixepoch() AND u.deleted_at IS NULL",
        [token], |r| Ok((CurrentUser { id: r.get(0)?, name: r.get(1)?, is_admin: r.get(2)?, theme: r.get(3)?, must_change_pw: r.get(4)? }, r.get::<_, i64>(5)?)),
    ).optional()?;
    if let Some((user, exp)) = u {
        // Sliding expiry, refreshed at most daily to avoid a write per request.
        c.execute("UPDATE session SET expires_at = unixepoch() + 30*86400 WHERE token = ?1 AND ?2 < unixepoch() + 29*86400", params![token, exp])?;
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
        let user = st.db.call(move |c| lookup_session(c, &token)).await?.ok_or(AppError::Unauthorized)?;
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
// login/logout/setup handlers: verify(), insert session with 32 random bytes (base64url),
// Set-Cookie: hs_session=…; HttpOnly; SameSite=Lax; Secure; Path=/; Max-Age=2592000.
// Rate-limit failed logins (5/min per IP) with an in-memory map.
