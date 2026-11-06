//! App passwords for DAV clients (decision 10): one per device or app,
//! shown once, revocable on its own, optionally scoped to named collections.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::access::{self, Access};
use super::principals::Principal;
use super::{DomainError, Result};

/// No `l`, `o`, `0` or `1`: easy to type from another screen.
const ALPHABET: &[u8; 32] = b"abcdefghijkmnpqrstuvwxyz23456789";
const GROUPS: usize = 5;
const GROUP_LEN: usize = 5;

/// What a scoped password may reach, per collection.
pub type Scope = HashMap<Uuid, Access>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPassword {
    pub id: Uuid,
    pub label: String,
    /// None: everything the user can reach.
    pub scope: Option<Scope>,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// A verified login.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Login {
    pub principal: Principal,
    pub password_id: Uuid,
    pub scope: Option<Scope>,
}

impl Login {
    /// The access this login has to a collection, given the principal's
    /// own: capped by the scope, None outside it.
    pub fn cap(&self, collection: Uuid, access: Access) -> Option<Access> {
        match &self.scope {
            None => Some(access),
            Some(s) => s.get(&collection).map(|limit| access.min(*limit)),
        }
    }
}

/// 25 characters from a 32-letter alphabet (125 bits), in groups of five.
pub fn generate() -> String {
    let mut bytes = [0u8; GROUPS * GROUP_LEN];
    getrandom::fill(&mut bytes).expect("the OS random source works");
    let mut out = String::with_capacity(GROUPS * (GROUP_LEN + 1));
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && i % GROUP_LEN == 0 {
            out.push('-');
        }
        out.push(ALPHABET[(*b & 31) as usize] as char);
    }
    out
}

/// Case, spaces and dashes do not matter, so a retyped password works.
fn normalise(password: &str) -> String {
    password.chars().filter(|c| !c.is_whitespace() && *c != '-').flat_map(char::to_lowercase).collect()
}

fn hash(password: &str) -> String {
    let digest = Sha256::digest(normalise(password).as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn level_name(a: Access) -> &'static str {
    if a.can_write() { "write" } else { "read" }
}

/// Create a password for a user. With a scope, every collection in it must
/// be visible to the user; the level is capped by the user's own access at
/// use time anyway. Returns the id and the password (shown once).
pub async fn create(pool: &PgPool, user: Uuid, label: &str, scope: Option<&Scope>) -> Result<(Uuid, String)> {
    let label = label.trim();
    if label.is_empty() || label.len() > 100 {
        return Err(DomainError::Invalid("a label of 1 to 100 characters".into()));
    }
    let mut tx = pool.begin().await?;
    let ok: Option<bool> =
        sqlx::query_scalar("SELECT TRUE FROM principals WHERE id = $1 AND kind = 'user' AND NOT disabled")
            .bind(user)
            .fetch_optional(&mut *tx)
            .await?;
    if ok.is_none() {
        return Err(DomainError::NotFound);
    }
    let id = Uuid::new_v4();
    let password = generate();
    sqlx::query("INSERT INTO app_passwords (id, principal_id, label, hash, scoped) VALUES ($1, $2, $3, $4, $5)")
        .bind(id)
        .bind(user)
        .bind(label)
        .bind(hash(&password))
        .bind(scope.is_some())
        .execute(&mut *tx)
        .await?;
    for (collection, level) in scope.into_iter().flatten() {
        if access::of(&mut *tx, user, *collection).await?.is_none() {
            return Err(DomainError::NotFound);
        }
        sqlx::query("INSERT INTO app_password_scopes (password_id, collection_id, level) VALUES ($1, $2, $3)")
            .bind(id)
            .bind(collection)
            .bind(level_name(*level))
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok((id, password))
}

async fn scope_of(pool: &PgPool, password: Uuid) -> Result<Scope> {
    let rows: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT collection_id, level FROM app_password_scopes WHERE password_id = $1")
            .bind(password)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().filter_map(|(c, l)| Some((c, Access::from_level(&l)?))).collect())
}

/// A user's passwords, newest first.
pub async fn list(pool: &PgPool, user: Uuid) -> Result<Vec<AppPassword>> {
    type Row = (Uuid, String, bool, DateTime<Utc>, Option<DateTime<Utc>>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, label, scoped, created_at, last_used_at FROM app_passwords
         WHERE principal_id = $1 ORDER BY created_at DESC, id",
    )
    .bind(user)
    .fetch_all(pool)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for (id, label, scoped, created_at, last_used_at) in rows {
        let scope = if scoped { Some(scope_of(pool, id).await?) } else { None };
        out.push(AppPassword { id, label, scope, created_at, last_used_at });
    }
    Ok(out)
}

/// Revoke one of the user's passwords; another user's id is `NotFound`.
pub async fn revoke(pool: &PgPool, user: Uuid, id: Uuid) -> Result<()> {
    let res = sqlx::query("DELETE FROM app_passwords WHERE id = $1 AND principal_id = $2")
        .bind(id)
        .bind(user)
        .execute(pool)
        .await?;
    if res.rows_affected() == 0 {
        return Err(DomainError::NotFound);
    }
    Ok(())
}

/// Check a user name and password. None for every kind of failure: unknown
/// user, wrong or revoked password, a group, a disabled account.
pub async fn verify(pool: &PgPool, username: &str, password: &str) -> Result<Option<Login>> {
    let row: Option<(Uuid, bool, Principal)> = sqlx::query_as::<_, (Uuid, bool, Uuid, String, String, String, bool)>(
        "SELECT a.id, a.scoped, p.id, p.kind, p.name, p.display_name, p.disabled
         FROM app_passwords a JOIN principals p ON p.id = a.principal_id
         WHERE a.hash = $1 AND p.name = $2 AND p.kind = 'user' AND NOT p.disabled",
    )
    .bind(hash(password))
    .bind(username.trim().to_ascii_lowercase())
    .fetch_optional(pool)
    .await?
    .map(|r| (r.0, r.1, Principal { id: r.2, kind: r.3, name: r.4, display_name: r.5, disabled: r.6 }));
    let Some((password_id, scoped, principal)) = row else { return Ok(None) };
    // At most one write a minute per password, so syncing stays read-only.
    sqlx::query(
        "UPDATE app_passwords SET last_used_at = now()
         WHERE id = $1 AND (last_used_at IS NULL OR last_used_at < now() - interval '1 minute')",
    )
    .bind(password_id)
    .execute(pool)
    .await?;
    let scope = if scoped { Some(scope_of(pool, password_id).await?) } else { None };
    Ok(Some(Login { principal, password_id, scope }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passwords_look_right_and_normalise() {
        let p = generate();
        assert_eq!(p.len(), 29);
        assert_eq!(p.matches('-').count(), 4);
        assert!(p.chars().all(|c| c == '-' || ALPHABET.contains(&(c as u8))));
        assert_ne!(generate(), p);
        assert_eq!(hash(&p), hash(&p.to_uppercase().replace('-', " ")));
        assert_ne!(hash(&p), hash(&generate()));
    }
}
