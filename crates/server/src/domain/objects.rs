//! Events, tasks and cards: conditional writes under the collection row lock,
//! the per-user layer, and the change log (decisions 5, 6 and 8).

use calzone_core::lines::Document;
use calzone_core::object::{self, Bounds};
use calzone_core::split::{self, UserLayer};
use chrono::NaiveDateTime;
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::access::{self, Access};
use super::collections::CollectionKind;
use super::{Actor, DomainError, Result, valid_resource_name};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectMeta {
    pub name: String,
    pub uid: String,
    /// `VEVENT`, `VTODO`, `VJOURNAL` or `VCARD`.
    pub kind: String,
    pub revision: i64,
}

impl ObjectMeta {
    /// The strong ETag, quotes included.
    pub fn etag(&self) -> String {
        etag(self.revision)
    }
}

pub fn etag(revision: i64) -> String {
    format!("\"{revision}\"")
}

/// An object as one actor sees it: the shared body with their own layer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Served {
    pub meta: ObjectMeta,
    pub body: String,
}

/// HTTP-style write conditions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Precondition {
    None,
    /// `If-Match: "<etag>"`: the object exists with this ETag.
    IfMatch(String),
    /// `If-None-Match: *`: the object does not exist yet.
    IfNoneMatchAny,
    /// `If-Match: *`: the object exists, whatever its ETag.
    IfMatchAny,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    pub meta: ObjectMeta,
    pub created: bool,
}

/// What changed in a collection since a sync token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Changes {
    /// The new token: the collection's sequence now.
    pub token: i64,
    pub changed: Vec<ObjectMeta>,
    pub deleted: Vec<String>,
}

pub(super) struct Locked {
    pub(super) kind: CollectionKind,
    pub(super) components: Vec<String>,
    /// The collection's sequence so far in this transaction: each write
    /// takes the next one, so two writes in one transaction stay distinct.
    pub(super) sequence: i64,
    pub(super) access: Access,
}

/// Lock the collection row and check access. Every write goes through this,
/// so writes to one collection are serialized and each takes its sequence
/// number inside the lock.
pub(super) async fn lock(tx: &mut Transaction<'_, Postgres>, actor: Actor, collection: Uuid) -> Result<Locked> {
    let row: Option<(String, Vec<String>, i64, bool)> = sqlx::query_as(
        "SELECT kind, components, sequence, EXISTS (SELECT 1 FROM subscriptions s WHERE s.collection_id = c.id)
         FROM collections c WHERE id = $1 FOR UPDATE",
    )
    .bind(collection)
    .fetch_optional(&mut **tx)
    .await?;
    let (kind, components, sequence, subscribed) = row.ok_or(DomainError::NotFound)?;
    let mut access = access::of(&mut **tx, actor, collection).await?.ok_or(DomainError::NotFound)?;
    if subscribed {
        // Filled from outside: nobody edits it (their own reminders aside,
        // which only need read access).
        access = Access::Read;
    }
    let kind = if kind == "addressbook" { CollectionKind::AddressBook } else { CollectionKind::Calendar };
    Ok(Locked { kind, components, sequence, access })
}

/// The refresher's lock on a subscribed calendar: no actor, full access.
pub(super) async fn lock_for_refresh(tx: &mut Transaction<'_, Postgres>, collection: Uuid) -> Result<Locked> {
    let row: Option<(String, Vec<String>, i64)> =
        sqlx::query_as("SELECT kind, components, sequence FROM collections WHERE id = $1 FOR UPDATE")
            .bind(collection)
            .fetch_optional(&mut **tx)
            .await?;
    let (kind, components, sequence) = row.ok_or(DomainError::NotFound)?;
    let kind = if kind == "addressbook" { CollectionKind::AddressBook } else { CollectionKind::Calendar };
    Ok(Locked { kind, components, sequence, access: Access::Owner })
}

async fn readable(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<Access> {
    access::of(pool, actor, collection).await?.ok_or(DomainError::NotFound)
}

/// What a body becomes before it is stored.
pub(super) struct Prepared {
    pub(super) uid: String,
    kind: &'static str,
    pub(super) shared: String,
    layer: Option<UserLayer>,
    undated: bool,
    starts_at: Option<NaiveDateTime>,
    ends_at: Option<NaiveDateTime>,
    recurring: bool,
    summary: Option<String>,
    emails: Vec<String>,
}

impl Prepared {
    /// The shared part only: storing it leaves every person's layer as it
    /// is (a feed's alarms are its publisher's, never ours).
    pub(super) fn without_layer(self) -> Prepared {
        Prepared { layer: None, ..self }
    }
}

pub(super) fn prepare(kind: CollectionKind, components: &[String], body: &str) -> Result<Prepared> {
    let mut doc = Document::parse(body).map_err(|e| DomainError::Invalid(e.to_string()))?;
    match kind {
        CollectionKind::Calendar => {
            if doc.components.first().is_some_and(|c| c.name == "VCARD") {
                return Err(DomainError::WrongKind);
            }
            let info = object::inspect_calendar(&doc, body).map_err(|e| DomainError::Invalid(e.to_string()))?;
            let name = info.kind.component_name();
            if !components.is_empty() && !components.iter().any(|c| c == name) {
                return Err(DomainError::ComponentNotAccepted);
            }
            let layer = split::split(&mut doc);
            let (undated, starts_at, ends_at) = match info.bounds {
                Bounds::Undated => (true, None, None),
                Bounds::Span { start, end } => (false, Some(start), end),
            };
            Ok(Prepared {
                uid: info.uid,
                kind: name,
                shared: doc.to_string(),
                layer: Some(layer),
                undated,
                starts_at,
                ends_at,
                recurring: info.recurring,
                summary: info.summary,
                emails: Vec::new(),
            })
        }
        CollectionKind::AddressBook => {
            if doc.components.first().is_some_and(|c| c.name != "VCARD") {
                return Err(DomainError::WrongKind);
            }
            let info = object::inspect_card(&doc).map_err(|e| DomainError::Invalid(e.to_string()))?;
            Ok(Prepared {
                // A card without UID is keyed by its resource name (filled in by put).
                uid: info.uid.unwrap_or_default(),
                kind: "VCARD",
                shared: body.to_string(),
                layer: None,
                undated: true,
                starts_at: None,
                ends_at: None,
                recurring: false,
                summary: info.formatted_name,
                emails: info.emails,
            })
        }
    }
}

/// Create or replace an object. The actor's personal items (alarms and their
/// client state) are stored as the actor's layer; everyone else's layers
/// stay as they were.
pub async fn put(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    body: &str,
    pre: Precondition,
) -> Result<Written> {
    if !valid_resource_name(name) {
        return Err(DomainError::BadName);
    }
    let mut tx = pool.begin().await?;
    let mut locked = lock(&mut tx, actor, collection).await?;
    if !locked.access.can_write() {
        return Err(DomainError::ReadOnly);
    }
    let mut p = prepare(locked.kind, &locked.components, body)?;
    if p.uid.is_empty() {
        p.uid = format!("name:{name}");
    }
    let w = store(&mut tx, &mut locked, actor, collection, name, p, pre).await?;
    tx.commit().await?;
    Ok(w)
}

/// How an imported object landed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upserted {
    Created,
    Updated,
    /// The actor already reads back exactly this body; nothing was written.
    Unchanged,
}

/// Write an imported object: under the name of the object with the same UID
/// when there is one, else under `name`. Nothing is written (no sequence
/// bump, no new ETag) when the actor would read back exactly `body`, so
/// re-running an import changes nothing (decision 30). Returns how it landed
/// and the name it has.
pub async fn upsert(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    body: &str,
) -> Result<(Upserted, String)> {
    if !valid_resource_name(name) {
        return Err(DomainError::BadName);
    }
    let mut tx = pool.begin().await?;
    let mut locked = lock(&mut tx, actor, collection).await?;
    if !locked.access.can_write() {
        return Err(DomainError::ReadOnly);
    }
    let mut p = prepare(locked.kind, &locked.components, body)?;
    if p.uid.is_empty() {
        p.uid = format!("name:{name}");
    }
    let existing: Option<(String, String, Option<Json<UserLayer>>)> = sqlx::query_as(
        "SELECT o.name, o.body, l.layer FROM objects o
         LEFT JOIN user_layers l ON l.object_id = o.id AND l.principal_id = $3
         WHERE o.collection_id = $1 AND o.uid = $2",
    )
    .bind(collection)
    .bind(&p.uid)
    .bind(actor)
    .fetch_optional(&mut *tx)
    .await?;
    let name = match existing {
        Some((existing, stored, layer)) => {
            if served_body(stored, layer) == body {
                return Ok((Upserted::Unchanged, existing));
            }
            existing
        }
        None => name.to_string(),
    };
    let w = store(&mut tx, &mut locked, actor, collection, &name, p, Precondition::None).await?;
    tx.commit().await?;
    Ok((if w.created { Upserted::Created } else { Upserted::Updated }, name))
}

/// The write itself, inside the collection lock: preconditions, the UID
/// check, the row, the actor's layer and the change log, then commit.
pub(super) async fn store(
    tx: &mut Transaction<'_, Postgres>,
    locked: &mut Locked,
    actor: Actor,
    collection: Uuid,
    name: &str,
    p: Prepared,
    pre: Precondition,
) -> Result<Written> {
    let existing: Option<(Uuid, i64)> =
        sqlx::query_as("SELECT id, revision FROM objects WHERE collection_id = $1 AND name = $2")
            .bind(collection)
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?;
    match (&pre, &existing) {
        (Precondition::IfMatch(want), Some((_, rev))) if *want == etag(*rev) => {}
        (Precondition::IfMatch(_), _) => return Err(DomainError::PreconditionFailed),
        (Precondition::IfMatchAny, None) => return Err(DomainError::PreconditionFailed),
        (Precondition::IfNoneMatchAny, Some(_)) => return Err(DomainError::PreconditionFailed),
        _ => {}
    }
    let clash: Option<String> =
        sqlx::query_scalar("SELECT name FROM objects WHERE collection_id = $1 AND uid = $2 AND name <> $3")
            .bind(collection)
            .bind(&p.uid)
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?;
    if let Some(existing) = clash {
        return Err(DomainError::UidConflict { existing });
    }

    let seq = locked.sequence + 1;
    locked.sequence = seq;
    sqlx::query("UPDATE collections SET sequence = $2 WHERE id = $1")
        .bind(collection)
        .bind(seq)
        .execute(&mut **tx)
        .await?;
    let id: Uuid = sqlx::query_scalar(
        "INSERT INTO objects (id, collection_id, name, uid, kind, body, revision, undated, starts_at, ends_at,
                              recurring, summary, emails)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
         ON CONFLICT (collection_id, name) DO UPDATE SET
            uid = EXCLUDED.uid, kind = EXCLUDED.kind, body = EXCLUDED.body, revision = EXCLUDED.revision,
            undated = EXCLUDED.undated, starts_at = EXCLUDED.starts_at, ends_at = EXCLUDED.ends_at,
            recurring = EXCLUDED.recurring, summary = EXCLUDED.summary, emails = EXCLUDED.emails,
            updated_at = now()
         RETURNING id",
    )
    .bind(existing.map(|e| e.0).unwrap_or_else(Uuid::new_v4))
    .bind(collection)
    .bind(name)
    .bind(&p.uid)
    .bind(p.kind)
    .bind(&p.shared)
    .bind(seq)
    .bind(p.undated)
    .bind(p.starts_at)
    .bind(p.ends_at)
    .bind(p.recurring)
    .bind(&p.summary)
    .bind(&p.emails)
    .fetch_one(&mut **tx)
    .await?;
    if let Some(layer) = &p.layer {
        if layer.entries.is_empty() {
            sqlx::query("DELETE FROM user_layers WHERE object_id = $1 AND principal_id = $2")
                .bind(id)
                .bind(actor)
                .execute(&mut **tx)
                .await?;
        } else {
            sqlx::query(
                "INSERT INTO user_layers (object_id, principal_id, layer) VALUES ($1, $2, $3)
                 ON CONFLICT (object_id, principal_id) DO UPDATE SET layer = EXCLUDED.layer",
            )
            .bind(id)
            .bind(actor)
            .bind(Json(layer))
            .execute(&mut **tx)
            .await?;
        }
    }
    sqlx::query("INSERT INTO changes (collection_id, sequence, name, deleted) VALUES ($1, $2, $3, FALSE)")
        .bind(collection)
        .bind(seq)
        .bind(name)
        .execute(&mut **tx)
        .await?;
    Ok(Written {
        meta: ObjectMeta { name: name.to_string(), uid: p.uid, kind: p.kind.to_string(), revision: seq },
        created: existing.is_none(),
    })
}

#[derive(sqlx::FromRow)]
struct ServedRow {
    name: String,
    uid: String,
    kind: String,
    body: String,
    revision: i64,
    layer: Option<Json<UserLayer>>,
}

impl From<ServedRow> for Served {
    fn from(r: ServedRow) -> Self {
        Served {
            meta: ObjectMeta { name: r.name, uid: r.uid, kind: r.kind, revision: r.revision },
            body: served_body(r.body, r.layer),
        }
    }
}

#[derive(sqlx::FromRow)]
struct ChangeRow {
    name: String,
    deleted: bool,
    uid: Option<String>,
    kind: Option<String>,
    revision: Option<i64>,
}

/// One object with the actor's own layer merged in.
pub async fn get(pool: &PgPool, actor: Actor, collection: Uuid, name: &str) -> Result<Served> {
    readable(pool, actor, collection).await?;
    let row: Option<ServedRow> = sqlx::query_as(
        "SELECT o.name, o.uid, o.kind, o.body, o.revision, l.layer FROM objects o
         LEFT JOIN user_layers l ON l.object_id = o.id AND l.principal_id = $3
         WHERE o.collection_id = $1 AND o.name = $2",
    )
    .bind(collection)
    .bind(name)
    .bind(actor)
    .fetch_optional(pool)
    .await?;
    Ok(row.ok_or(DomainError::NotFound)?.into())
}

pub(super) fn served_body(body: String, layer: Option<Json<UserLayer>>) -> String {
    let Some(Json(layer)) = layer else { return body };
    match Document::parse(&body) {
        Ok(mut doc) => {
            split::merge(&mut doc, &layer);
            doc.to_string()
        }
        // Stored bodies always parse (put checked them); never fail a read.
        Err(_) => body,
    }
}

/// Several objects by name (CalDAV multiget); missing names are left out.
pub async fn get_many(pool: &PgPool, actor: Actor, collection: Uuid, names: &[String]) -> Result<Vec<Served>> {
    readable(pool, actor, collection).await?;
    let rows: Vec<ServedRow> = sqlx::query_as(
        "SELECT o.name, o.uid, o.kind, o.body, o.revision, l.layer FROM objects o
         LEFT JOIN user_layers l ON l.object_id = o.id AND l.principal_id = $3
         WHERE o.collection_id = $1 AND o.name = ANY($2) ORDER BY o.name",
    )
    .bind(collection)
    .bind(names)
    .bind(actor)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Served::from).collect())
}

/// Every object as the actor sees it (exports), in name order.
pub async fn all(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<Vec<Served>> {
    readable(pool, actor, collection).await?;
    let rows: Vec<ServedRow> = sqlx::query_as(
        "SELECT o.name, o.uid, o.kind, o.body, o.revision, l.layer FROM objects o
         LEFT JOIN user_layers l ON l.object_id = o.id AND l.principal_id = $2
         WHERE o.collection_id = $1 ORDER BY o.name",
    )
    .bind(collection)
    .bind(actor)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(Served::from).collect())
}

/// Every object's name and ETag.
pub async fn list(pool: &PgPool, actor: Actor, collection: Uuid) -> Result<Vec<ObjectMeta>> {
    readable(pool, actor, collection).await?;
    let rows: Vec<(String, String, String, i64)> =
        sqlx::query_as("SELECT name, uid, kind, revision FROM objects WHERE collection_id = $1 ORDER BY name")
            .bind(collection)
            .fetch_all(pool)
            .await?;
    Ok(rows.into_iter().map(|(name, uid, kind, revision)| ObjectMeta { name, uid, kind, revision }).collect())
}

/// Candidates for a time range `[from, to)` by projection (UTC). The bounds
/// are widened for floating times, so this never misses an object; callers
/// that need exact answers expand the candidates with the core.
pub async fn in_range(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    from: NaiveDateTime,
    to: NaiveDateTime,
) -> Result<Vec<ObjectMeta>> {
    readable(pool, actor, collection).await?;
    let rows: Vec<(String, String, String, i64)> = sqlx::query_as(
        "SELECT name, uid, kind, revision FROM objects
         WHERE collection_id = $1 AND kind <> 'VCARD'
           AND (undated OR (starts_at < $3 AND (ends_at IS NULL OR ends_at > $2 OR (ends_at = starts_at AND starts_at >= $2))))
         ORDER BY name",
    )
    .bind(collection)
    .bind(from)
    .bind(to)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|(name, uid, kind, revision)| ObjectMeta { name, uid, kind, revision }).collect())
}

/// Delete an object; it stays in the change log as a tombstone.
pub async fn delete(pool: &PgPool, actor: Actor, collection: Uuid, name: &str, pre: Precondition) -> Result<()> {
    let mut tx = pool.begin().await?;
    let mut locked = lock(&mut tx, actor, collection).await?;
    delete_in(&mut tx, &mut locked, collection, name, pre).await?;
    tx.commit().await?;
    Ok(())
}

/// The delete itself, inside the collection lock (no commit).
pub(super) async fn delete_in(
    tx: &mut Transaction<'_, Postgres>,
    locked: &mut Locked,
    collection: Uuid,
    name: &str,
    pre: Precondition,
) -> Result<()> {
    let existing: Option<i64> =
        sqlx::query_scalar("SELECT revision FROM objects WHERE collection_id = $1 AND name = $2")
            .bind(collection)
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?;
    let Some(rev) = existing else { return Err(DomainError::NotFound) };
    if !locked.access.can_write() {
        return Err(DomainError::ReadOnly);
    }
    match pre {
        Precondition::IfMatch(want) if want != etag(rev) => return Err(DomainError::PreconditionFailed),
        Precondition::IfNoneMatchAny => return Err(DomainError::PreconditionFailed),
        _ => {}
    }
    let seq = locked.sequence + 1;
    locked.sequence = seq;
    sqlx::query("UPDATE collections SET sequence = $2 WHERE id = $1")
        .bind(collection)
        .bind(seq)
        .execute(&mut **tx)
        .await?;
    sqlx::query("DELETE FROM objects WHERE collection_id = $1 AND name = $2")
        .bind(collection)
        .bind(name)
        .execute(&mut **tx)
        .await?;
    sqlx::query("INSERT INTO changes (collection_id, sequence, name, deleted) VALUES ($1, $2, $3, TRUE)")
        .bind(collection)
        .bind(seq)
        .bind(name)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// What changed since `since` (None: everything that exists now). The
/// answer is one consistent snapshot, read in a repeatable-read transaction.
pub async fn changes_since(pool: &PgPool, actor: Actor, collection: Uuid, since: Option<i64>) -> Result<Changes> {
    readable(pool, actor, collection).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
    let token: i64 = sqlx::query_scalar("SELECT sequence FROM collections WHERE id = $1")
        .bind(collection)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(DomainError::NotFound)?;
    let since = match since {
        Some(s) if s < 0 || s > token => return Err(DomainError::BadSyncToken),
        Some(s) => s,
        None => 0,
    };
    // The latest change per name after the token; deleted ones are tombstones,
    // the rest are joined to the object as it is now.
    let rows: Vec<ChangeRow> = sqlx::query_as(
        "SELECT ch.name, ch.deleted, o.uid, o.kind, o.revision FROM (
            SELECT DISTINCT ON (name) name, deleted FROM changes
            WHERE collection_id = $1 AND sequence > $2 ORDER BY name, sequence DESC
         ) ch LEFT JOIN objects o ON o.collection_id = $1 AND o.name = ch.name
         ORDER BY ch.name",
    )
    .bind(collection)
    .bind(since)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    let mut out = Changes { token, changed: Vec::new(), deleted: Vec::new() };
    for r in rows {
        let name = r.name;
        match (r.deleted, r.uid, r.kind, r.revision) {
            (false, Some(uid), Some(kind), Some(revision)) => {
                out.changed.push(ObjectMeta { name, uid, kind, revision })
            }
            // A full sync does not need to hear about things that are gone.
            _ if since == 0 => {}
            _ => out.deleted.push(name),
        }
    }
    Ok(out)
}
