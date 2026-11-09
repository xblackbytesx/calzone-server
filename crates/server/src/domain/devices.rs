//! Phones with the CalZone app (decision 10): a one-time sign-in code shown
//! as a QR code on the Devices page, redeemed by the app for a device token.
//! Codes and tokens are 256 random bits, shown once, stored only as
//! SHA-256. Each phone is its own row, signed out on its own.

use base64::Engine as _;
use chrono::{DateTime, Duration, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::{DomainError, Result};

/// Device tokens: `Authorization: Bearer czd_...`.
pub const TOKEN_PREFIX: &str = "czd_";
/// Sign-in codes, in the QR code's `/link/czl_...` address.
pub const LINK_PREFIX: &str = "czl_";
/// How long a sign-in code works: long enough to find the phone, short
/// enough that a photo of the screen is soon useless.
pub const LINK_MINUTES: i64 = 10;

fn secret(prefix: &str) -> String {
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("the OS random source works");
    format!("{prefix}{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf))
}

/// Plain SHA-256 is enough for 256-bit random secrets.
fn hash(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

/// A secret of ours: the prefix, then 43 base64url characters.
fn well_formed(s: &str, prefix: &str) -> bool {
    s.strip_prefix(prefix)
        .is_some_and(|r| r.len() == 43 && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: Uuid,
    pub label: String,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
    /// A push endpoint is registered.
    pub push: bool,
}

/// A signed-in device, as the sync API sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedIn {
    pub device: Uuid,
    pub principal: Uuid,
}

/// A new sign-in code for a user (from a signed-in web session, so no
/// second factor is asked again). Returns the code and when it expires.
pub async fn create_link(pool: &PgPool, user: Uuid) -> Result<(String, DateTime<Utc>)> {
    let ok: Option<bool> =
        sqlx::query_scalar("SELECT TRUE FROM principals WHERE id = $1 AND kind = 'user' AND NOT disabled")
            .bind(user)
            .fetch_optional(pool)
            .await?;
    if ok.is_none() {
        return Err(DomainError::NotFound);
    }
    sqlx::query("DELETE FROM device_links WHERE expires_at < now()").execute(pool).await?;
    let code = secret(LINK_PREFIX);
    let expires = Utc::now() + Duration::minutes(LINK_MINUTES);
    sqlx::query("INSERT INTO device_links (code_sha256, principal_id, expires_at) VALUES ($1, $2, $3)")
        .bind(hash(&code))
        .bind(user)
        .bind(expires)
        .execute(pool)
        .await?;
    Ok((code, expires))
}

/// The phone's own name for itself: trimmed, control characters out, at
/// most 100 characters; empty becomes "Phone".
fn clean_label(label: &str) -> String {
    let l: String = label.chars().filter(|c| !c.is_control()).take(100).collect();
    let l = l.trim();
    if l.is_empty() { "Phone".to_string() } else { l.to_string() }
}

/// Redeem a sign-in code: a new device for its user, and the token (shown
/// once). None when the code is unknown, used, expired, or its user is
/// disabled; the code is used up in the same statement, so two phones
/// racing on one code cannot both get in.
pub async fn claim(pool: &PgPool, code: &str, label: &str) -> Result<Option<(SignedIn, String)>> {
    if !well_formed(code.trim(), LINK_PREFIX) {
        return Ok(None);
    }
    let mut tx = pool.begin().await?;
    let user: Option<Uuid> = sqlx::query_scalar(
        "UPDATE device_links l SET used_at = now() FROM principals p
         WHERE l.code_sha256 = $1 AND l.used_at IS NULL AND l.expires_at > now()
           AND p.id = l.principal_id AND p.kind = 'user' AND NOT p.disabled
         RETURNING l.principal_id",
    )
    .bind(hash(code.trim()))
    .fetch_optional(&mut *tx)
    .await?;
    let Some(user) = user else { return Ok(None) };
    let id = Uuid::new_v4();
    let token = secret(TOKEN_PREFIX);
    sqlx::query("INSERT INTO devices (id, principal_id, label, token_sha256) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(user)
        .bind(clean_label(label))
        .bind(hash(&token))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some((SignedIn { device: id, principal: user }, token)))
}

/// The device behind a token, if it is still signed in and its user is
/// enabled. Records use at most once a minute, so syncing stays cheap.
pub async fn verify(pool: &PgPool, token: &str) -> Result<Option<SignedIn>> {
    if !well_formed(token, TOKEN_PREFIX) {
        return Ok(None);
    }
    let row: Option<(Uuid, Uuid, bool)> = sqlx::query_as(
        "SELECT d.id, d.principal_id, d.last_used_at IS NULL OR d.last_used_at < now() - interval '1 minute'
         FROM devices d JOIN principals p ON p.id = d.principal_id
         WHERE d.token_sha256 = $1 AND p.kind = 'user' AND NOT p.disabled",
    )
    .bind(hash(token))
    .fetch_optional(pool)
    .await?;
    let Some((device, principal, stale)) = row else { return Ok(None) };
    if stale {
        sqlx::query("UPDATE devices SET last_used_at = now() WHERE id = $1").bind(device).execute(pool).await?;
    }
    Ok(Some(SignedIn { device, principal }))
}

/// A user's devices, newest first.
pub async fn list(pool: &PgPool, user: Uuid) -> Result<Vec<Device>> {
    type Row = (Uuid, String, DateTime<Utc>, Option<DateTime<Utc>>, bool);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, label, created_at, last_used_at, push_url IS NOT NULL FROM devices
         WHERE principal_id = $1 ORDER BY created_at DESC, id",
    )
    .bind(user)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, label, created_at, last_used_at, push)| Device { id, label, created_at, last_used_at, push })
        .collect())
}

/// One device of a user.
pub async fn get(pool: &PgPool, user: Uuid, id: Uuid) -> Result<Device> {
    list(pool, user).await?.into_iter().find(|d| d.id == id).ok_or(DomainError::NotFound)
}

/// Sign a device out (from the web, or the phone itself); another user's
/// device is `NotFound`. Its push endpoint goes with it.
pub async fn remove(pool: &PgPool, user: Uuid, id: Uuid) -> Result<()> {
    let res = sqlx::query("DELETE FROM devices WHERE id = $1 AND principal_id = $2")
        .bind(id)
        .bind(user)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(DomainError::NotFound);
    }
    Ok(())
}

/// Store this device's push endpoint and WebPush keys (checked by the
/// caller), replacing any earlier one.
pub async fn set_push(pool: &PgPool, device: Uuid, url: &str, key: &str, auth: &str) -> Result<()> {
    sqlx::query("UPDATE devices SET push_url = $2, push_key = $3, push_auth = $4 WHERE id = $1")
        .bind(device)
        .bind(url)
        .bind(key)
        .bind(auth)
        .execute(pool)
        .await?;
    Ok(())
}

/// Forget this device's push endpoint. With `only_url`, only while it is
/// still that one (a push server said it is gone, and the phone may have
/// registered a new one meanwhile).
pub async fn clear_push(pool: &PgPool, device: Uuid, only_url: Option<&str>) -> Result<()> {
    sqlx::query(
        "UPDATE devices SET push_url = NULL, push_key = NULL, push_auth = NULL
         WHERE id = $1 AND ($2::text IS NULL OR push_url = $2)",
    )
    .bind(device)
    .bind(only_url)
    .execute(pool)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secrets_and_labels() {
        let t = secret(TOKEN_PREFIX);
        assert!(well_formed(&t, TOKEN_PREFIX));
        assert!(!well_formed(&t, LINK_PREFIX));
        assert!(!well_formed("czd_short", TOKEN_PREFIX));
        assert_ne!(secret(TOKEN_PREFIX), t);
        assert_eq!(clean_label("  Pixel 9\u{7}\n "), "Pixel 9");
        assert_eq!(clean_label(" "), "Phone");
        assert_eq!(clean_label(&"x".repeat(300)).len(), 100);
    }
}
