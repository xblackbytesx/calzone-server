//! Web logins (decision 10): Argon2id passphrases, admin flags, and browser
//! sessions. Sessions live here, like every other table, so the web door
//! never touches the database itself.

use argon2::Argon2;
use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use base64::Engine as _;
use chrono::{Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::{DomainError, Result};

pub const MIN_PASSPHRASE: usize = 10;
const MAX_PASSPHRASE: usize = 1024;
pub const SESSION_DAYS: i64 = 30;
/// Without "keep me signed in" (a shared or work computer).
pub const SHORT_SESSION_HOURS: i64 = 12;
/// Between the passphrase and the two-factor code.
pub const PENDING_MINUTES: i64 = 10;

/// A valid Argon2id hash of a random string: unknown users still pay one
/// verification, so timing does not tell which names exist.
const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$c29tZXNhbHRzb21lc2FsdA$9sTbSlTio3Biev89thdrlKKiCaYsjjYVJxGAL3swxpQ";

fn hash_blocking(passphrase: &str) -> String {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default().hash_password(passphrase.as_bytes(), &salt).expect("argon2 hashing").to_string()
}

fn verify_blocking(passphrase: &str, stored: &str) -> bool {
    PasswordHash::new(stored).is_ok_and(|h| Argon2::default().verify_password(passphrase.as_bytes(), &h).is_ok())
}

/// Argon2 off the async workers.
async fn hash(passphrase: String) -> String {
    tokio::task::spawn_blocking(move || hash_blocking(&passphrase)).await.expect("hashing task")
}

async fn verify(passphrase: String, stored: String) -> bool {
    tokio::task::spawn_blocking(move || verify_blocking(&passphrase, &stored)).await.unwrap_or(false)
}

/// A user as the people page shows it.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Account {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub display_name: String,
    pub disabled: bool,
    pub is_admin: bool,
    pub has_passphrase: bool,
    /// Two-factor sign-in is set up (decision 32).
    pub has_totp: bool,
}

impl Account {
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() { &self.name } else { &self.display_name }
    }

    pub fn is_group(&self) -> bool {
        self.kind == "group"
    }
}

const ACCOUNT_COLUMNS: &str = "id, kind, name, display_name, disabled, is_admin, passphrase_hash IS NOT NULL AS has_passphrase, \
     totp_secret IS NOT NULL AS has_totp";

/// Every user and group, users first, by name.
pub async fn list(pool: &PgPool) -> Result<Vec<Account>> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM principals ORDER BY kind DESC, name");
    Ok(sqlx::query_as(&sql).fetch_all(pool).await?)
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Account> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM principals WHERE id = $1");
    sqlx::query_as(&sql).bind(id).fetch_optional(pool).await?.ok_or(DomainError::NotFound)
}

/// The user id for a name and passphrase. None for every failure: unknown,
/// a group, disabled, no passphrase yet, wrong passphrase. Always one Argon2
/// verification.
pub async fn check_login(pool: &PgPool, name: &str, passphrase: &str) -> Result<Option<Uuid>> {
    let row: Option<(Uuid, Option<String>)> =
        sqlx::query_as("SELECT id, passphrase_hash FROM principals WHERE name = $1 AND kind = 'user' AND NOT disabled")
            .bind(name.trim().to_ascii_lowercase())
            .fetch_optional(pool)
            .await?;
    match row {
        Some((id, Some(stored))) => Ok(verify(passphrase.to_string(), stored).await.then_some(id)),
        _ => {
            verify(passphrase.to_string(), DUMMY_HASH.to_string()).await;
            Ok(None)
        }
    }
}

/// Whether `passphrase` is the user's current one.
pub async fn passphrase_matches(pool: &PgPool, user: Uuid, passphrase: &str) -> Result<bool> {
    let stored: Option<Option<String>> = sqlx::query_scalar("SELECT passphrase_hash FROM principals WHERE id = $1")
        .bind(user)
        .fetch_optional(pool)
        .await?;
    Ok(match stored.flatten() {
        Some(s) => verify(passphrase.to_string(), s).await,
        None => false,
    })
}

/// Set a user's passphrase and end all their sessions.
pub async fn set_passphrase(pool: &PgPool, user: Uuid, passphrase: &str) -> Result<()> {
    let n = passphrase.chars().count();
    if n < MIN_PASSPHRASE || passphrase.len() > MAX_PASSPHRASE {
        return Err(DomainError::Invalid(format!("a passphrase needs at least {MIN_PASSPHRASE} characters")));
    }
    let h = hash(passphrase.to_string()).await;
    let res = sqlx::query("UPDATE principals SET passphrase_hash = $2 WHERE id = $1 AND kind = 'user'")
        .bind(user)
        .bind(h)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(DomainError::NotFound);
    }
    sqlx::query("DELETE FROM web_sessions WHERE principal_id = $1").bind(user).execute(pool).await?;
    Ok(())
}

/// A temporary passphrase (shown once): for a new user, or a forgotten one.
pub async fn issue_temporary(pool: &PgPool, user: Uuid) -> Result<String> {
    let p = super::app_passwords::generate();
    set_passphrase(pool, user, &p).await?;
    Ok(p)
}

pub async fn set_admin(pool: &PgPool, user: Uuid, admin: bool) -> Result<()> {
    let res = sqlx::query("UPDATE principals SET is_admin = $2 WHERE id = $1 AND kind = 'user'")
        .bind(user)
        .bind(admin)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(DomainError::NotFound);
    }
    Ok(())
}

pub async fn set_display_name(pool: &PgPool, id: Uuid, display_name: &str) -> Result<()> {
    let name = display_name.trim();
    if name.chars().count() > 100 {
        return Err(DomainError::Invalid("a name of at most 100 characters".into()));
    }
    sqlx::query("UPDATE principals SET display_name = $2 WHERE id = $1").bind(id).bind(name).execute(pool).await?;
    Ok(())
}

fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// Start a session; returns the cookie value (256 random bits). Persistent:
/// 30 days; otherwise 12 hours.
pub async fn create_session(pool: &PgPool, user: Uuid, persistent: bool) -> Result<String> {
    let life = if persistent { Duration::days(SESSION_DAYS) } else { Duration::hours(SHORT_SESSION_HOURS) };
    insert_session(pool, user, true, persistent, life).await
}

/// A session that only reaches the two-factor code page, for a few minutes.
/// It remembers whether the session after it should be persistent.
pub async fn create_pending_session(pool: &PgPool, user: Uuid, persistent: bool) -> Result<String> {
    insert_session(pool, user, false, persistent, Duration::minutes(PENDING_MINUTES)).await
}

/// Whether a session (pending or not) was started with "keep me signed in".
pub async fn session_persistent(pool: &PgPool, token: &str) -> Result<bool> {
    let p: Option<bool> = sqlx::query_scalar("SELECT persistent FROM web_sessions WHERE token_sha256 = $1")
        .bind(token_hash(token))
        .fetch_optional(pool)
        .await?;
    Ok(p.unwrap_or(false))
}

async fn insert_session(pool: &PgPool, user: Uuid, verified: bool, persistent: bool, life: Duration) -> Result<String> {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("the OS random source works");
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    sqlx::query(
        "INSERT INTO web_sessions (token_sha256, principal_id, expires_at, verified, persistent) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(token_hash(&token))
    .bind(user)
    .bind(Utc::now() + life)
    .bind(verified)
    .bind(persistent)
        .execute(pool)
        .await?;
    // Opportunistic cleanup keeps the table small without a scheduler.
    sqlx::query("DELETE FROM web_sessions WHERE expires_at < now()").execute(pool).await?;
    Ok(token)
}

/// The signed-in user for a session cookie, if it is live and the user is
/// still enabled.
pub async fn session_user(pool: &PgPool, token: &str) -> Result<Option<Account>> {
    // No column names clash between the two tables.
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM web_sessions s JOIN principals p ON p.id = s.principal_id
         WHERE s.token_sha256 = $1 AND s.expires_at > now() AND NOT p.disabled AND p.kind = 'user' AND s.verified"
    );
    Ok(sqlx::query_as(&sql).bind(token_hash(token)).fetch_optional(pool).await?)
}

/// The user behind a pending (passphrase only) session.
pub async fn pending_user(pool: &PgPool, token: &str) -> Result<Option<Account>> {
    let sql = format!(
        "SELECT {ACCOUNT_COLUMNS} FROM web_sessions s JOIN principals p ON p.id = s.principal_id
         WHERE s.token_sha256 = $1 AND s.expires_at > now() AND NOT p.disabled AND p.kind = 'user' AND NOT s.verified"
    );
    Ok(sqlx::query_as(&sql).bind(token_hash(token)).fetch_optional(pool).await?)
}

pub async fn end_session(pool: &PgPool, token: &str) -> Result<()> {
    sqlx::query("DELETE FROM web_sessions WHERE token_sha256 = $1").bind(token_hash(token)).execute(pool).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_verify() {
        let h = hash_blocking("correct horse battery");
        assert!(h.starts_with("$argon2id$"));
        assert!(verify_blocking("correct horse battery", &h));
        assert!(!verify_blocking("wrong", &h));
        assert!(!verify_blocking("x", "not a hash"));
        assert!(PasswordHash::new(DUMMY_HASH).is_ok(), "the timing dummy must parse");
    }
}
