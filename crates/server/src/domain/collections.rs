//! Calendars and address books, and the grants that share them.

use sqlx::PgPool;
use uuid::Uuid;

use super::access::{self, Access};
use super::{Actor, DomainError, Result, valid_slug};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionKind {
    Calendar,
    AddressBook,
}

impl CollectionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CollectionKind::Calendar => "calendar",
            CollectionKind::AddressBook => "addressbook",
        }
    }

    fn parse(s: &str) -> Self {
        if s == "addressbook" { CollectionKind::AddressBook } else { CollectionKind::Calendar }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Collection {
    pub id: Uuid,
    pub owner_id: Uuid,
    pub owner_name: String,
    pub kind: CollectionKind,
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub color: Option<String>,
    /// Calendars: accepted component types; empty accepts all.
    pub components: Vec<String>,
    /// The change log's head: the sync token and CTag.
    pub sequence: i64,
    /// Filled from an outside `.ics` (decision 24): read-only for everyone,
    /// whatever their access.
    pub subscribed: bool,
    /// The asking actor's access.
    pub access: Access,
}

#[derive(Debug, Clone)]
pub struct NewCollection<'a> {
    pub kind: CollectionKind,
    pub slug: &'a str,
    pub display_name: &'a str,
    pub description: &'a str,
    pub color: Option<&'a str>,
    pub components: &'a [&'a str],
}

type Row = (Uuid, Uuid, String, String, String, String, String, Option<String>, Vec<String>, i64, bool, Option<i32>);

const COLUMNS: &str = "c.id, c.owner_id, o.name, c.kind, c.slug, c.display_name, c.description, c.color, c.components, c.sequence, EXISTS (SELECT 1 FROM subscriptions s WHERE s.collection_id = c.id)";

fn from_row(r: Row) -> Option<Collection> {
    Some(Collection {
        id: r.0,
        owner_id: r.1,
        owner_name: r.2,
        kind: CollectionKind::parse(&r.3),
        slug: r.4,
        display_name: r.5,
        description: r.6,
        color: r.7,
        components: r.8,
        sequence: r.9,
        subscribed: r.10,
        access: access::from_number(r.11)?,
    })
}

/// Create a collection owned by the actor.
pub async fn create(pool: &PgPool, actor: Actor, new: NewCollection<'_>) -> Result<Collection> {
    if !valid_slug(new.slug) {
        return Err(DomainError::BadName);
    }
    for c in new.components {
        if !matches!(*c, "VEVENT" | "VTODO" | "VJOURNAL") {
            return Err(DomainError::Invalid(format!("unknown component type {c}")));
        }
    }
    let components: Vec<String> = match new.kind {
        CollectionKind::Calendar => new.components.iter().map(|s| s.to_string()).collect(),
        CollectionKind::AddressBook => Vec::new(),
    };
    let id = Uuid::new_v4();
    let res = sqlx::query(
        "INSERT INTO collections (id, owner_id, kind, slug, display_name, description, color, components)
         SELECT $1, p.id, $3, $4, $5, $6, $7, $8 FROM principals p WHERE p.id = $2 AND NOT p.disabled",
    )
    .bind(id)
    .bind(actor)
    .bind(new.kind.as_str())
    .bind(new.slug)
    .bind(new.display_name)
    .bind(new.description)
    .bind(new.color)
    .bind(&components)
    .execute(pool)
    .await;
    match res {
        Ok(r) if r.rows_affected() == 1 => get(pool, actor, id).await,
        Ok(_) => Err(DomainError::NotFound),
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => Err(DomainError::NameTaken),
        Err(e) => Err(e.into()),
    }
}

/// A URL segment from a display name: `Family 2026!` becomes `family-2026`.
pub fn slugify(name: &str, fallback: &str) -> String {
    let mut out = String::new();
    for c in name.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_end_matches('-').to_string();
    if out.is_empty() { fallback.to_string() } else { out }
}

/// Create a collection whose slug comes from `base` (a slug already); a
/// taken slug gets a number (`family-2`, `family-3`, ...).
pub async fn create_numbered(pool: &PgPool, actor: Actor, base: &str, new: NewCollection<'_>) -> Result<Collection> {
    for n in 1..=50 {
        let slug = if n == 1 { base.to_string() } else { format!("{base}-{n}") };
        match create(pool, actor, NewCollection { slug: &slug, ..new.clone() }).await {
            Err(DomainError::NameTaken) => continue,
            other => return other,
        }
    }
    Err(DomainError::NameTaken)
}

/// One collection the actor can see.
pub async fn get(pool: &PgPool, actor: Actor, id: Uuid) -> Result<Collection> {
    let sql = format!(
        "SELECT {COLUMNS}, ({}) FROM collections c JOIN principals o ON o.id = c.owner_id WHERE c.id = $2",
        access::level_sql("$1")
    );
    let row: Option<Row> = sqlx::query_as(&sql).bind(actor).bind(id).fetch_optional(pool).await?;
    row.and_then(from_row).ok_or(DomainError::NotFound)
}

/// A collection by its path (`/<owner>/<slug>`), if the actor can see it.
pub async fn by_path(pool: &PgPool, actor: Actor, owner: &str, slug: &str) -> Result<Collection> {
    let sql = format!(
        "SELECT {COLUMNS}, ({}) FROM collections c JOIN principals o ON o.id = c.owner_id
         WHERE o.name = $2 AND c.slug = $3",
        access::level_sql("$1")
    );
    let row: Option<Row> =
        sqlx::query_as(&sql).bind(actor).bind(owner.to_ascii_lowercase()).bind(slug).fetch_optional(pool).await?;
    row.and_then(from_row).ok_or(DomainError::NotFound)
}

/// Everything the actor owns or was granted, of one kind, owned first.
pub async fn visible(pool: &PgPool, actor: Actor, kind: CollectionKind) -> Result<Vec<Collection>> {
    let sql = format!(
        "SELECT * FROM (
            SELECT {COLUMNS}, ({}) AS lvl FROM collections c JOIN principals o ON o.id = c.owner_id
            WHERE c.kind = $2
         ) v WHERE v.lvl IS NOT NULL ORDER BY v.lvl DESC, v.name, v.slug",
        access::level_sql("$1")
    );
    let rows: Vec<Row> = sqlx::query_as(&sql).bind(actor).bind(kind.as_str()).fetch_all(pool).await?;
    Ok(rows.into_iter().filter_map(from_row).collect())
}

/// The gap every collection's numbering jumps by in [`start_over`]: more
/// writes than a restored backup can be behind by.
pub const RESYNC_GAP: i64 = 1_000_000;

/// After restoring a backup: renumber every collection's change log,
/// revisions and undo history past a gap, and refuse sync states from
/// before (decision 48). Every phone and DAV client then syncs from the
/// start, so none keeps something the restored server no longer has.
/// Returns how many collections. Run with the server stopped or idle.
pub async fn start_over(pool: &PgPool) -> Result<u64> {
    let mut tx = pool.begin().await?;
    sqlx::query("LOCK TABLE collections IN EXCLUSIVE MODE").execute(&mut *tx).await?;
    // Each collection shifts by its own sequence plus the gap, so new
    // numbers never meet old ones (no unique clash mid-update).
    let shift = format!("c.sequence + {RESYNC_GAP}");
    sqlx::query(&format!(
        "UPDATE changes ch SET sequence = ch.sequence + {shift} FROM collections c WHERE ch.collection_id = c.id"
    ))
    .execute(&mut *tx)
    .await?;
    sqlx::query(&format!(
        "UPDATE objects o SET revision = o.revision + {shift} FROM collections c WHERE o.collection_id = c.id"
    ))
    .execute(&mut *tx)
    .await?;
    sqlx::query(&format!(
        "UPDATE object_history h SET after_revision = h.after_revision + {shift}
         FROM collections c WHERE h.collection_id = c.id AND h.after_revision IS NOT NULL"
    ))
    .execute(&mut *tx)
    .await?;
    let n = sqlx::query(&format!(
        "UPDATE collections SET floor = sequence + {RESYNC_GAP}, sequence = 2 * sequence + {RESYNC_GAP}"
    ))
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(n)
}

/// Display name, description and color (admin or owner).
pub async fn update_properties(
    pool: &PgPool,
    actor: Actor,
    id: Uuid,
    display_name: Option<&str>,
    description: Option<&str>,
    color: Option<Option<&str>>,
) -> Result<Collection> {
    let c = get(pool, actor, id).await?;
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly);
    }
    sqlx::query(
        "UPDATE collections SET display_name = COALESCE($2, display_name), description = COALESCE($3, description),
         color = CASE WHEN $4 THEN $5 ELSE color END WHERE id = $1",
    )
    .bind(id)
    .bind(display_name)
    .bind(description)
    .bind(color.is_some())
    .bind(color.flatten())
    .execute(pool)
    .await?;
    get(pool, actor, id).await
}

/// Delete a collection and everything in it (owner or admin).
pub async fn delete(pool: &PgPool, actor: Actor, id: Uuid) -> Result<()> {
    let c = get(pool, actor, id).await?;
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly);
    }
    sqlx::query("DELETE FROM collections WHERE id = $1").bind(id).execute(pool).await?;
    Ok(())
}

/// Someone a collection is shared with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub principal_id: Uuid,
    pub kind: String,
    pub name: String,
    pub display_name: String,
    pub level: Access,
}

/// Who a collection is shared with (owner or admin).
pub async fn grants(pool: &PgPool, actor: Actor, id: Uuid) -> Result<Vec<Grant>> {
    let c = get(pool, actor, id).await?;
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly);
    }
    let rows: Vec<(Uuid, String, String, String, String)> = sqlx::query_as(
        "SELECT p.id, p.kind, p.name, p.display_name, g.level FROM grants g JOIN principals p ON p.id = g.principal_id
         WHERE g.collection_id = $1 ORDER BY p.kind DESC, p.name",
    )
    .bind(id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(principal_id, kind, name, display_name, level)| {
            Some(Grant { principal_id, kind, name, display_name, level: Access::from_level(&level)? })
        })
        .collect())
}

/// Share a collection with a principal at a level (owner or admin). Level
/// `None` removes the grant. The owner's own access cannot be changed.
pub async fn set_grant(pool: &PgPool, actor: Actor, id: Uuid, principal: Uuid, level: Option<Access>) -> Result<()> {
    let c = get(pool, actor, id).await?;
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly);
    }
    if principal == c.owner_id {
        return Err(DomainError::Invalid("the owner always has full access".into()));
    }
    match level {
        None => {
            sqlx::query("DELETE FROM grants WHERE collection_id = $1 AND principal_id = $2")
                .bind(id)
                .bind(principal)
                .execute(pool)
                .await?;
        }
        Some(level) => {
            let level = match level {
                Access::Read => "read",
                Access::Write => "write",
                Access::Admin | Access::Owner => "admin",
            };
            sqlx::query(
                "INSERT INTO grants (collection_id, principal_id, level) VALUES ($1, $2, $3)
                 ON CONFLICT (collection_id, principal_id) DO UPDATE SET level = EXCLUDED.level",
            )
            .bind(id)
            .bind(principal)
            .bind(level)
            .execute(pool)
            .await?;
        }
    }
    Ok(())
}
