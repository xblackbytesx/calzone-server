//! Publishing (decision 24): secret links that serve one calendar as
//! `.ics`, full or busy-only. Only the token's SHA-256 is stored; whoever
//! may share a calendar may make and revoke its links. A revoked or unknown
//! token is simply not found.

use base64::Engine as _;
use calzone_core::feed::{self, Detail};
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use uuid::Uuid;

use super::{Actor, CollectionKind, DomainError, Result, collections};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedLink {
    pub id: Uuid,
    pub collection_id: Uuid,
    pub label: String,
    pub busy: bool,
    pub created_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

/// id, label, detail, created, last used.
type LinkRow = (Uuid, String, String, DateTime<Utc>, Option<DateTime<Utc>>);
/// link, calendar, detail, display name, slug, sequence, maker's language.
type FeedRow = (Uuid, Uuid, String, String, String, i64, Option<String>);

fn hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

async fn admin_calendar(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<super::Collection> {
    let c = collections::get(pool, actor, collection).await?;
    if c.kind != CollectionKind::Calendar {
        return Err(DomainError::NotFound);
    }
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly);
    }
    Ok(c)
}

/// A new link; the token (shown once) is returned with it.
pub async fn create_link(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    label: &str,
    detail: Detail,
) -> Result<(FeedLink, String)> {
    admin_calendar(pool, actor, collection).await?;
    if label.chars().count() > 100 {
        return Err(DomainError::Invalid("a label of at most 100 characters".into()));
    }
    let mut buf = [0u8; 32];
    getrandom::fill(&mut buf).expect("the OS random source works");
    let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
    let id = Uuid::new_v4();
    let busy = detail == Detail::Busy;
    let created_at: DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO feed_links (id, collection_id, created_by, token_sha256, detail, label) VALUES ($1, $2, $3, $4, $5, $6)
         RETURNING created_at",
    )
    .bind(id)
    .bind(collection)
    .bind(actor)
    .bind(hash(&token))
    .bind(if busy { "busy" } else { "full" })
    .bind(label.trim())
    .fetch_one(pool)
    .await?;
    Ok((
        FeedLink {
            id,
            collection_id: collection,
            label: label.trim().to_string(),
            busy,
            created_at,
            last_used_at: None,
        },
        token,
    ))
}

pub async fn links(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<Vec<FeedLink>> {
    admin_calendar(pool, actor, collection).await?;
    let rows: Vec<LinkRow> = sqlx::query_as(
        "SELECT id, label, detail, created_at, last_used_at FROM feed_links WHERE collection_id = $1 ORDER BY created_at",
    )
    .bind(collection)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, label, detail, created_at, last_used_at)| FeedLink {
            id,
            collection_id: collection,
            label,
            busy: detail == "busy",
            created_at,
            last_used_at,
        })
        .collect())
}

pub async fn revoke(pool: &PgPool, actor: Actor, link: Uuid) -> Result<()> {
    let collection: Option<Uuid> = sqlx::query_scalar("SELECT collection_id FROM feed_links WHERE id = $1")
        .bind(link)
        .fetch_optional(pool)
        .await?;
    let collection = collection.ok_or(DomainError::NotFound)?;
    admin_calendar(pool, actor, collection).await?;
    sqlx::query("DELETE FROM feed_links WHERE id = $1").bind(link).execute(pool).await?;
    Ok(())
}

/// A feed as served.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Published {
    pub body: String,
    /// Strong, quotes included: changes with every write to the calendar.
    pub etag: String,
    pub last_modified: Option<DateTime<Utc>>,
    pub file_name: String,
}

/// The feed behind a token, or `NotFound`. `busy_title` names busy
/// entries from the language setting of whoever made the link (feed
/// readers send no useful language). Shared bodies only: no one's reminders.
pub async fn published(pool: &PgPool, token: &str, busy_title: fn(Option<&str>) -> &'static str) -> Result<Published> {
    let row: Option<FeedRow> = sqlx::query_as(
        "SELECT l.id, c.id, l.detail, c.display_name, c.slug, c.sequence, cr.language FROM feed_links l
         JOIN collections c ON c.id = l.collection_id
         JOIN principals o ON o.id = c.owner_id
         LEFT JOIN principals cr ON cr.id = l.created_by
         WHERE l.token_sha256 = $1 AND c.kind = 'calendar' AND NOT o.disabled",
    )
    .bind(hash(token))
    .fetch_optional(pool)
    .await?;
    let (link, collection, detail, display, slug, sequence, language) = row.ok_or(DomainError::NotFound)?;
    sqlx::query("UPDATE feed_links SET last_used_at = now() WHERE id = $1").bind(link).execute(pool).await?;
    let bodies: Vec<String> =
        sqlx::query_scalar("SELECT body FROM objects WHERE collection_id = $1 AND kind = 'VEVENT' ORDER BY name")
            .bind(collection)
            .fetch_all(pool)
            .await?;
    let last_modified: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT max(updated_at) FROM objects WHERE collection_id = $1")
            .bind(collection)
            .fetch_one(pool)
            .await?;
    let detail = if detail == "busy" { Detail::Busy } else { Detail::Full };
    let name = if display.is_empty() { slug.clone() } else { display };
    Ok(Published {
        body: feed::publish(&name, &bodies, detail, busy_title(language.as_deref())),
        etag: format!("\"{sequence}-{}\"", if detail == Detail::Busy { "busy" } else { "full" }),
        last_modified,
        file_name: format!("{slug}.ics"),
    })
}
