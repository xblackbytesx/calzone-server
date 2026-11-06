//! Subscriptions (decision 24): calendars filled from an outside `.ics`
//! address and refreshed on a schedule. Read-only for everyone (the object
//! lock says so); only the refresher writes, through [`replace`], which
//! changes nothing unless every incoming object is valid (the last good
//! copy is kept, AGENTS invariant). Failures back off, doubling up to a day.
//! People's own reminders on subscribed events survive every refresh.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::objects::{Precondition, delete_in, lock_for_refresh, prepare, store};
use super::{Actor, Collection, CollectionKind, DomainError, NewCollection, Result, collections};

/// Refresh intervals people can pick, in minutes.
pub const INTERVALS: [i32; 4] = [15, 60, 360, 1440];
const MAX_BACKOFF_MINUTES: i64 = 1440;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Subscription {
    pub collection_id: Uuid,
    pub url: String,
    pub interval_minutes: i32,
    pub next_at: DateTime<Utc>,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_error: String,
    pub failures: i32,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
}

const COLUMNS: &str = "collection_id, url, interval_minutes, next_at, last_attempt_at, last_success_at, last_error, failures, etag, last_modified";

/// Make a calendar for an outside address. The caller fetches it first and
/// only then calls this, so a broken address never leaves an empty calendar.
pub async fn create(
    pool: &PgPool,
    actor: Actor,
    url: &str,
    name: &str,
    color: Option<&str>,
    interval: i32,
) -> Result<Collection> {
    if !INTERVALS.contains(&interval) {
        return Err(DomainError::Invalid("an interval of 15, 60, 360 or 1440 minutes".into()));
    }
    let new = NewCollection {
        kind: CollectionKind::Calendar,
        slug: "",
        display_name: name,
        description: "",
        color,
        components: &[],
    };
    let c = collections::create_numbered(pool, actor, &collections::slugify(name, "subscribed"), new).await?;
    sqlx::query("INSERT INTO subscriptions (collection_id, url, interval_minutes) VALUES ($1, $2, $3)")
        .bind(c.id)
        .bind(url)
        .bind(interval)
        .execute(pool)
        .await?;
    collections::get(pool, actor, c.id).await
}

/// One subscription the actor can see.
pub async fn get(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<Subscription> {
    collections::get(pool, actor, collection).await?;
    by_id(pool, collection).await
}

async fn by_id(pool: &PgPool, collection: Uuid) -> Result<Subscription> {
    let row: Option<Subscription> =
        sqlx::query_as(&format!("SELECT {COLUMNS} FROM subscriptions WHERE collection_id = $1"))
            .bind(collection)
            .fetch_optional(pool)
            .await?;
    row.ok_or(DomainError::NotFound)
}

/// Due for a refresh now, oldest first.
pub async fn due(pool: &PgPool, limit: i64) -> Result<Vec<Subscription>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {COLUMNS} FROM subscriptions s WHERE next_at <= now()
           AND EXISTS (SELECT 1 FROM collections c JOIN principals o ON o.id = c.owner_id WHERE c.id = s.collection_id AND NOT o.disabled)
         ORDER BY next_at LIMIT $1"
    ))
    .bind(limit)
    .fetch_all(pool)
    .await?)
}

pub async fn set_interval(pool: &PgPool, actor: Actor, collection: Uuid, minutes: i32) -> Result<()> {
    let c = collections::get(pool, actor, collection).await?;
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly);
    }
    if !INTERVALS.contains(&minutes) {
        return Err(DomainError::Invalid("an interval of 15, 60, 360 or 1440 minutes".into()));
    }
    let r = sqlx::query(
        "UPDATE subscriptions SET interval_minutes = $2,
            next_at = LEAST(next_at, COALESCE(last_attempt_at, now()) + make_interval(mins => $2))
         WHERE collection_id = $1",
    )
    .bind(collection)
    .bind(minutes)
    .execute(pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(DomainError::NotFound);
    }
    Ok(())
}

/// The outcome of a refresh attempt, recorded for the page and the schedule.
pub async fn record_success(
    pool: &PgPool,
    collection: Uuid,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<()> {
    sqlx::query(
        "UPDATE subscriptions SET last_attempt_at = now(), last_success_at = now(), last_error = '', failures = 0,
            etag = COALESCE($2, etag), last_modified = COALESCE($3, last_modified),
            next_at = now() + make_interval(mins => interval_minutes)
         WHERE collection_id = $1",
    )
    .bind(collection)
    .bind(etag)
    .bind(last_modified)
    .execute(pool)
    .await?;
    Ok(())
}

/// A failed attempt: the reason (plain words, never content), and the next
/// try after twice the last wait, at most a day.
pub async fn record_failure(pool: &PgPool, collection: Uuid, reason: &str) -> Result<()> {
    let sub = by_id(pool, collection).await?;
    let failures = sub.failures + 1;
    // Twice the wait per failure in a row: interval, 2x, 4x, ... capped.
    let doublings = (failures - 1).clamp(0, 10) as u32;
    let wait = (i64::from(sub.interval_minutes) << doublings).min(MAX_BACKOFF_MINUTES);
    let reason: String = reason.chars().take(300).collect();
    sqlx::query(
        "UPDATE subscriptions SET last_attempt_at = now(), last_error = $2, failures = $3, next_at = now() + make_interval(mins => $4)
         WHERE collection_id = $1",
    )
    .bind(collection)
    .bind(reason)
    .bind(failures)
    .bind(wait as i32)
    .execute(pool)
    .await?;
    Ok(())
}

/// Ask for a refresh at the next round (the page's "refresh now" runs one
/// itself; this is for the schedule).
pub async fn wake(pool: &PgPool, collection: Uuid) -> Result<()> {
    sqlx::query("UPDATE subscriptions SET next_at = now() WHERE collection_id = $1")
        .bind(collection)
        .execute(pool)
        .await?;
    Ok(())
}

/// What a replace changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Replaced {
    pub added: u32,
    pub changed: u32,
    pub removed: u32,
    pub unchanged: u32,
}

fn name_for(uid: &str) -> String {
    let h: String = Sha256::digest(uid.as_bytes()).iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("{h}.ics")
}

/// Make the calendar exactly the incoming objects (one per UID), in one
/// transaction. Every object is checked before anything is written: one
/// invalid object refuses the whole copy (`Invalid`), and an empty copy is
/// refused too, so the last good copy stays.
pub async fn replace(pool: &PgPool, collection: Uuid, objects: &[String]) -> Result<Replaced> {
    if objects.is_empty() {
        return Err(DomainError::Invalid("the feed has no events; the last good copy is kept".into()));
    }
    let owner: Uuid = sqlx::query_scalar("SELECT owner_id FROM collections WHERE id = $1")
        .bind(collection)
        .fetch_optional(pool)
        .await?
        .ok_or(DomainError::NotFound)?;
    let mut tx = pool.begin().await?;
    let mut locked = lock_for_refresh(&mut tx, collection).await?;
    let mut prepared = Vec::with_capacity(objects.len());
    for (i, o) in objects.iter().enumerate() {
        let p = prepare(locked.kind, &locked.components, o).map_err(|e| {
            DomainError::Invalid(format!("entry {} does not read ({e}); the last good copy is kept", i + 1))
        })?;
        // Nobody's reminders change with a refresh: the feed's own alarms
        // are dropped, and stored layers stay.
        prepared.push(p.without_layer());
    }
    let existing: Vec<(String, String, String)> =
        sqlx::query_as("SELECT name, uid, body FROM objects WHERE collection_id = $1")
            .bind(collection)
            .fetch_all(&mut *tx)
            .await?;
    let by_uid: HashMap<&str, (&str, &str)> =
        existing.iter().map(|(n, u, b)| (u.as_str(), (n.as_str(), b.as_str()))).collect();
    let mut seen = HashSet::new();
    let mut out = Replaced::default();
    for p in prepared {
        if !seen.insert(p.uid.clone()) {
            continue;
        }
        match by_uid.get(p.uid.as_str()) {
            Some((_, body)) if *body == p.shared => out.unchanged += 1,
            Some((name, _)) => {
                let name = name.to_string();
                store(&mut tx, &mut locked, owner, collection, &name, p, Precondition::None).await?;
                out.changed += 1;
            }
            None => {
                let name = name_for(&p.uid);
                store(&mut tx, &mut locked, owner, collection, &name, p, Precondition::None).await?;
                out.added += 1;
            }
        }
    }
    for (name, uid, _) in &existing {
        if !seen.contains(uid) {
            delete_in(&mut tx, &mut locked, collection, name, Precondition::None).await?;
            out.removed += 1;
        }
    }
    tx.commit().await?;
    Ok(out)
}
