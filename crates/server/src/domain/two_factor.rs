//! Two-factor sign-in for the web app (decision 32): TOTP codes from an
//! authenticator app (RFC 6238: HMAC-SHA1, 6 digits, 30 seconds, one step
//! of clock drift either way), each code accepted once, plus ten single-use
//! recovery codes. Secrets are stored encrypted with AES-256-GCM under
//! `CALZONE_SECRET_KEY`, bound to the principal's id.

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use hmac::{Hmac, Mac};
use sha1::Sha1;
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::{DomainError, Result};

/// The server key for secrets at rest.
pub type SecretKey = [u8; 32];

pub const STEP_SECONDS: i64 = 30;
const SECRET_LEN: usize = 20;
const RECOVERY_CODES: usize = 10;
/// Same alphabet as app passwords: no `l`, `o`, `0` or `1`.
const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";

/// The 6-digit code for one time step.
pub fn code_at(secret: &[u8], step: i64) -> u32 {
    let mut mac = <Hmac<Sha1> as Mac>::new_from_slice(secret).expect("hmac takes any key length");
    mac.update(&(step as u64).to_be_bytes());
    let h = mac.finalize().into_bytes();
    let off = (h[19] & 0x0f) as usize;
    let bin = (u32::from(h[off] & 0x7f) << 24)
        | (u32::from(h[off + 1]) << 16)
        | (u32::from(h[off + 2]) << 8)
        | u32::from(h[off + 3]);
    bin % 1_000_000
}

pub fn step_at(unix: i64) -> i64 {
    unix.div_euclid(STEP_SECONDS)
}

/// RFC 4648 base32 without padding, as authenticator apps expect.
pub fn base32(bytes: &[u8]) -> String {
    const A: &[u8; 32] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let (mut buf, mut bits) = (0u32, 0);
    for &b in bytes {
        buf = (buf << 8) | u32::from(b);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(A[((buf >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(A[((buf << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// The `otpauth://` link the QR code carries.
pub fn otpauth_uri(account: &str, secret: &[u8]) -> String {
    let label: String = account
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }
        })
        .collect();
    format!("otpauth://totp/CalZone:{label}?secret={}&issuer=CalZone&algorithm=SHA1&digits=6&period=30", base32(secret))
}

fn encrypt(key: &SecretKey, user: Uuid, plain: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new_from_slice(key).expect("32-byte key");
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut nonce).expect("the OS random source works");
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad: user.as_bytes() }).expect("aes-gcm");
    [nonce.as_slice(), &ct].concat()
}

/// None when the key is not the one it was stored with (or the row was
/// tampered with): the person has to set two-factor up again.
fn decrypt(key: &SecretKey, user: Uuid, stored: &[u8]) -> Option<Vec<u8>> {
    if stored.len() < 12 {
        return None;
    }
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let (nonce, ct) = stored.split_at(12);
    cipher.decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad: user.as_bytes() }).ok()
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("the OS random source works");
    b
}

/// `abcde-fghij`: 50 random bits.
fn new_recovery_code() -> String {
    let b = random_bytes::<10>();
    let s: String = b.iter().map(|x| ALPHABET[(x & 31) as usize] as char).collect();
    format!("{}-{}", &s[..5], &s[5..])
}

fn normalize(code: &str) -> String {
    code.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_lowercase()).collect()
}

fn code_hash(code: &str) -> Vec<u8> {
    Sha256::digest(normalize(code).as_bytes()).to_vec()
}

/// Start (or continue) setup: the pending secret, made on first call and
/// kept, so reloading the setup page shows the same QR code.
pub async fn begin_setup(pool: &PgPool, key: &SecretKey, user: Uuid) -> Result<Vec<u8>> {
    let pending: Option<Option<Vec<u8>>> =
        sqlx::query_scalar("SELECT totp_pending FROM principals WHERE id = $1").bind(user).fetch_optional(pool).await?;
    let pending = pending.ok_or(DomainError::NotFound)?;
    if let Some(secret) = pending.and_then(|p| decrypt(key, user, &p)) {
        return Ok(secret);
    }
    let secret = random_bytes::<SECRET_LEN>().to_vec();
    sqlx::query("UPDATE principals SET totp_pending = $2 WHERE id = $1")
        .bind(user)
        .bind(encrypt(key, user, &secret))
        .execute(pool)
        .await?;
    Ok(secret)
}

/// Which step, if any, `code` is valid for around `now`.
fn matching_step(secret: &[u8], code: &str, now_unix: i64) -> Option<i64> {
    let digits = normalize(code);
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let want: u32 = digits.parse().ok()?;
    let now = step_at(now_unix);
    [now - 1, now, now + 1].into_iter().find(|&s| code_at(secret, s) == want)
}

/// Finish setup with a first code: two-factor is on, the recovery codes
/// (returned once) replace any old ones, and every other session ends,
/// since those were signed in without a code.
pub async fn confirm_setup(
    pool: &PgPool,
    key: &SecretKey,
    user: Uuid,
    code: &str,
    now_unix: i64,
    keep_session: &str,
) -> Result<Option<Vec<String>>> {
    let pending: Option<Vec<u8>> = sqlx::query_scalar("SELECT totp_pending FROM principals WHERE id = $1")
        .bind(user)
        .fetch_optional(pool)
        .await?
        .flatten();
    let Some(secret) = pending.and_then(|p| decrypt(key, user, &p)) else { return Ok(None) };
    let Some(step) = matching_step(&secret, code, now_unix) else { return Ok(None) };
    let codes: Vec<String> = (0..RECOVERY_CODES).map(|_| new_recovery_code()).collect();
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE principals SET totp_secret = $2, totp_pending = NULL, totp_last_step = $3 WHERE id = $1")
        .bind(user)
        .bind(encrypt(key, user, &secret))
        .bind(step)
        .execute(&mut *tx)
        .await?;
    replace_codes(&mut tx, user, &codes).await?;
    sqlx::query("DELETE FROM web_sessions WHERE principal_id = $1 AND token_sha256 <> $2")
        .bind(user)
        .bind(Sha256::digest(keep_session.as_bytes()).to_vec())
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(Some(codes))
}

async fn replace_codes(tx: &mut sqlx::Transaction<'_, sqlx::Postgres>, user: Uuid, codes: &[String]) -> Result<()> {
    sqlx::query("DELETE FROM recovery_codes WHERE principal_id = $1").bind(user).execute(&mut **tx).await?;
    for c in codes {
        sqlx::query("INSERT INTO recovery_codes (principal_id, code_sha256) VALUES ($1, $2)")
            .bind(user)
            .bind(code_hash(c))
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

/// How a code was accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Accepted {
    App,
    RecoveryCode,
}

/// Check a code from the app (accepted once per time step) or an unused
/// recovery code (used up). None: wrong, reused, or two-factor is off.
pub async fn check(pool: &PgPool, key: &SecretKey, user: Uuid, code: &str, now_unix: i64) -> Result<Option<Accepted>> {
    let stored: Option<Vec<u8>> = sqlx::query_scalar("SELECT totp_secret FROM principals WHERE id = $1")
        .bind(user)
        .fetch_optional(pool)
        .await?
        .flatten();
    let Some(stored) = stored else { return Ok(None) };
    if let Some(secret) = decrypt(key, user, &stored)
        && let Some(step) = matching_step(&secret, code, now_unix)
    {
        // Atomic: two requests with the same code cannot both pass.
        let r = sqlx::query(
            "UPDATE principals SET totp_last_step = $2 WHERE id = $1 AND (totp_last_step IS NULL OR totp_last_step < $2)",
        )
        .bind(user)
        .bind(step)
        .execute(pool)
        .await?;
        return Ok((r.rows_affected() == 1).then_some(Accepted::App));
    }
    let n = normalize(code);
    if n.len() != 10 {
        return Ok(None);
    }
    let r = sqlx::query(
        "UPDATE recovery_codes SET used_at = now() WHERE principal_id = $1 AND code_sha256 = $2 AND used_at IS NULL",
    )
    .bind(user)
    .bind(code_hash(&n))
    .execute(pool)
    .await?;
    Ok((r.rows_affected() == 1).then_some(Accepted::RecoveryCode))
}

/// New recovery codes (returned once); the old ones stop working.
pub async fn new_recovery_codes(pool: &PgPool, user: Uuid) -> Result<Vec<String>> {
    let codes: Vec<String> = (0..RECOVERY_CODES).map(|_| new_recovery_code()).collect();
    let mut tx = pool.begin().await?;
    replace_codes(&mut tx, user, &codes).await?;
    tx.commit().await?;
    Ok(codes)
}

pub async fn unused_recovery_codes(pool: &PgPool, user: Uuid) -> Result<i64> {
    Ok(sqlx::query_scalar("SELECT count(*) FROM recovery_codes WHERE principal_id = $1 AND used_at IS NULL")
        .bind(user)
        .fetch_one(pool)
        .await?)
}

/// Turn two-factor off (by the person, or an admin's reset): the secret
/// and the codes go, and every session ends.
pub async fn disable(pool: &PgPool, user: Uuid) -> Result<()> {
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE principals SET totp_secret = NULL, totp_pending = NULL, totp_last_step = NULL WHERE id = $1")
        .bind(user)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM recovery_codes WHERE principal_id = $1").bind(user).execute(&mut *tx).await?;
    sqlx::query("DELETE FROM web_sessions WHERE principal_id = $1").bind(user).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

/// The setup QR code as inline SVG (no style attributes, so the CSP allows
/// it), dark on light whatever the page theme.
pub fn qr_svg(uri: &str) -> String {
    let code = qrcode::QrCode::new(uri.as_bytes()).expect("an otpauth URI fits a QR code");
    let svg = code
        .render::<qrcode::render::svg::Color>()
        .min_dimensions(220, 220)
        .dark_color(qrcode::render::svg::Color("#000000"))
        .light_color(qrcode::render::svg::Color("#ffffff"))
        .build();
    // Drop the XML declaration; the rest is an <svg> element.
    match svg.find("<svg") {
        Some(i) => svg[i..].to_string(),
        None => svg,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc_6238_vectors() {
        // RFC 6238 appendix B (SHA-1 seed), last 6 of the 8 digits.
        let secret = b"12345678901234567890";
        assert_eq!(code_at(secret, step_at(59)), 287_082);
        assert_eq!(code_at(secret, step_at(1_111_111_109)), 81_804);
        assert_eq!(code_at(secret, step_at(1_234_567_890)), 5_924);
        assert_eq!(code_at(secret, step_at(2_000_000_000)), 279_037);
    }

    #[test]
    fn base32_and_uri() {
        assert_eq!(base32(b"12345678901234567890"), "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ");
        assert_eq!(base32(b"f"), "MY");
        let u = otpauth_uri("anna b", b"12345678901234567890");
        assert!(u.starts_with("otpauth://totp/CalZone:anna%20b?secret=GEZDGNBV"));
        assert!(u.contains("issuer=CalZone") && u.contains("period=30"));
    }

    #[test]
    fn drift_window_and_format() {
        let s = b"12345678901234567890";
        let t = 1_111_111_109;
        let now = format!("{:06}", code_at(s, step_at(t)));
        let prev = format!("{:06}", code_at(s, step_at(t) - 1));
        let old = format!("{:06}", code_at(s, step_at(t) - 2));
        assert_eq!(matching_step(s, &now, t), Some(step_at(t)));
        assert_eq!(matching_step(s, &format!("{} {}", &now[..3], &now[3..]), t), Some(step_at(t)), "spaces are fine");
        assert_eq!(matching_step(s, &prev, t), Some(step_at(t) - 1));
        assert_eq!(matching_step(s, &old, t), None, "two steps old is too old");
        assert_eq!(matching_step(s, "12345", t), None);
    }

    #[test]
    fn secrets_are_bound_to_key_and_user() {
        let (k1, k2) = ([7u8; 32], [8u8; 32]);
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let sealed = encrypt(&k1, a, b"secret");
        assert_eq!(decrypt(&k1, a, &sealed).as_deref(), Some(&b"secret"[..]));
        assert_eq!(decrypt(&k2, a, &sealed), None, "another key");
        assert_eq!(decrypt(&k1, b, &sealed), None, "another user's row");
    }

    #[test]
    fn recovery_codes_and_qr() {
        let c = new_recovery_code();
        assert_eq!(c.len(), 11);
        assert_eq!(code_hash(&c.to_uppercase().replace('-', " ")), code_hash(&c));
        let svg = qr_svg("otpauth://totp/CalZone:anna?secret=ABC");
        assert!(svg.starts_with("<svg") && !svg.contains("style"), "inline SVG the CSP allows");
    }
}
