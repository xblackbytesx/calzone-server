//! The sync API's domain side (decision 15): the collections a person can
//! see with their own view of each, the changes since a state in batches,
//! and the phone's edits merged with the core's three-way merge (decision
//! 46). Everything a phone reads carries the person's own layer (decision
//! 8), and everything it writes goes through the same lock, checks and
//! change log as every other door (decisions 6 and 27).

use calzone_core::split::UserLayer;
use calzone_core::sync::{self as wire, Answer, Batch, Change, Push, Pushed, Reason, Rejected, State};
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::access::Access;
use super::collections::{self, CollectionKind};
use super::objects::{self, Locked, Precondition, served_body};
use super::{Actor, DomainError, Result, prefs, valid_resource_name};

/// At most this many bytes of bodies in one batch (at least one change is
/// always sent, whatever its size).
pub const BATCH_BYTES: usize = 4 * 1024 * 1024;

fn wire_access(a: Access) -> wire::Access {
    match a {
        Access::Read => wire::Access::Read,
        Access::Write => wire::Access::Write,
        Access::Admin => wire::Access::Admin,
        Access::Owner => wire::Access::Owner,
    }
}

/// Every calendar and address book the actor can see, with their own view.
pub async fn collections(pool: &PgPool, actor: Actor) -> Result<wire::Collections> {
    let mut all = collections::visible(pool, actor, CollectionKind::Calendar).await?;
    all.extend(collections::visible(pool, actor, CollectionKind::AddressBook).await?);
    let mut prefs = prefs::calendar_prefs(pool, actor).await?;
    let collections = all
        .into_iter()
        .map(|c| {
            let p = prefs.remove(&c.id).unwrap_or_default();
            wire::Collection {
                id: c.id.to_string(),
                kind: c.kind.as_str().to_string(),
                owner: c.owner_name.clone(),
                owner_name: String::new(),
                name: if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() },
                description: c.description,
                color: c.color,
                components: c.components,
                access: wire_access(c.access),
                read_only: c.subscribed || !c.access.can_write(),
                subscribed: c.subscribed,
                state: State(c.sequence),
                mine: wire::Mine {
                    color: p.color,
                    name: p.display_name,
                    hidden: p.hidden,
                    order: p.sort_order,
                    default_alarm: p.default_alarm,
                },
            }
        })
        .collect::<Vec<_>>();
    let mut out = wire::Collections { collections };
    // Owners' display names, in one query.
    let owners: Vec<String> = out.collections.iter().map(|c| c.owner.clone()).collect();
    let names: Vec<(String, String)> = sqlx::query_as("SELECT name, display_name FROM principals WHERE name = ANY($1)")
        .bind(&owners)
        .fetch_all(pool)
        .await?;
    for c in &mut out.collections {
        if let Some((_, d)) = names.iter().find(|(n, _)| *n == c.owner) {
            c.owner_name = if d.is_empty() { c.owner.clone() } else { d.clone() };
        }
    }
    Ok(out)
}

#[derive(sqlx::FromRow)]
struct ChangeRow {
    name: String,
    seq: i64,
    body: Option<String>,
    layer: Option<Json<UserLayer>>,
}

/// The changes to a collection after `since`, oldest first, at most
/// `limit` of them and about [`BATCH_BYTES`] of text. `None` is a full
/// sync: everything that exists, no tombstones. Each object comes once, at
/// its newest revision; when the batch is cut short, its state is the
/// newest revision in it, so asking again from there misses nothing. One
/// consistent snapshot (repeatable read).
pub async fn changes(pool: &PgPool, actor: Actor, collection: Uuid, since: Option<i64>, limit: usize) -> Result<Batch> {
    super::access::of(pool, actor, collection).await?.ok_or(DomainError::NotFound)?;
    let limit = limit.clamp(1, 1000);
    let mut tx = pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
    let token: i64 = sqlx::query_scalar("SELECT sequence FROM collections WHERE id = $1")
        .bind(collection)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(DomainError::NotFound)?;
    let from = match since {
        Some(s) if s < 0 || s > token => return Err(DomainError::BadSyncToken),
        Some(s) => s,
        None => 0,
    };
    let rows: Vec<ChangeRow> = sqlx::query_as(
        "SELECT ch.name, ch.seq, o.body, l.layer FROM (
            SELECT DISTINCT ON (name) name, sequence AS seq FROM changes
            WHERE collection_id = $1 AND sequence > $2 ORDER BY name, sequence DESC
         ) ch
         LEFT JOIN objects o ON o.collection_id = $1 AND o.name = ch.name
         LEFT JOIN user_layers l ON l.object_id = o.id AND l.principal_id = $3
         WHERE NOT ($4 AND o.body IS NULL)
         ORDER BY ch.seq LIMIT $5",
    )
    .bind(collection)
    .bind(from)
    .bind(actor)
    .bind(since.is_none())
    .bind(limit as i64 + 1)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;

    let mut out = Batch {
        collection: collection.to_string(),
        since: State(from),
        state: State(token),
        changes: Vec::new(),
        more: false,
    };
    let mut bytes = 0;
    for r in rows {
        let full = out.changes.len() == limit || (!out.changes.is_empty() && bytes >= BATCH_BYTES);
        if full {
            out.more = true;
            break;
        }
        let body = r.body.map(|b| served_body(b, r.layer));
        bytes += body.as_ref().map_or(0, String::len);
        out.changes.push(Change { name: r.name, revision: State(r.seq), body });
    }
    if out.more {
        out.state = out.changes.last().map(|c| c.revision).unwrap_or(State(from));
    }
    Ok(out)
}

/// The object as the actor sees it, its stored shared body and revision.
async fn current(
    tx: &mut Transaction<'_, Postgres>,
    actor: Actor,
    collection: Uuid,
    name: &str,
) -> Result<Option<(String, String, i64)>> {
    let row: Option<(String, i64, Option<Json<UserLayer>>)> = sqlx::query_as(
        "SELECT o.body, o.revision, l.layer FROM objects o
         LEFT JOIN user_layers l ON l.object_id = o.id AND l.principal_id = $3
         WHERE o.collection_id = $1 AND o.name = $2",
    )
    .bind(collection)
    .bind(name)
    .bind(actor)
    .fetch_optional(&mut **tx)
    .await?;
    Ok(row.map(|(shared, rev, layer)| (served_body(shared.clone(), layer), shared, rev)))
}

/// Why a write was refused, for the phone; `None` for a real failure.
fn reason(e: &DomainError) -> Option<Reason> {
    match e {
        DomainError::ReadOnly => Some(Reason::ReadOnly),
        DomainError::Invalid(_) | DomainError::WrongKind | DomainError::ComponentNotAccepted => Some(Reason::Invalid),
        DomainError::UidConflict { .. } => Some(Reason::UidTaken),
        DomainError::BadName => Some(Reason::BadName),
        _ => None,
    }
}

/// The phone's edits to one collection, merged and written together under
/// the collection lock. Each edit is merged with the actor's current copy
/// (decision 46): the server's value wins a same-field clash and the
/// phone's comes back as a conflict. An edit that changes nothing writes
/// nothing. With read access only, an edit may change the actor's own
/// reminders and nothing else. A refused edit is reported and the others
/// still go through.
pub async fn upload(pool: &PgPool, actor: Actor, collection: Uuid, pushes: &[Push]) -> Result<Answer> {
    let mut tx = pool.begin().await?;
    let mut locked = objects::lock(&mut tx, actor, collection).await?;
    let mut answer = Answer::default();
    for push in pushes {
        match apply(&mut tx, &mut locked, actor, collection, push).await {
            Ok(p) => answer.pushed.push(p),
            Err(e) => match reason(&e) {
                Some(reason) => {
                    answer.rejected.push(Rejected { name: push.name.clone(), reason, message: e.to_string() })
                }
                None => return Err(e),
            },
        }
    }
    tx.commit().await?;
    Ok(answer)
}

async fn apply(
    tx: &mut Transaction<'_, Postgres>,
    locked: &mut Locked,
    actor: Actor,
    collection: Uuid,
    push: &Push,
) -> Result<Pushed> {
    if !valid_resource_name(&push.name) {
        return Err(DomainError::BadName);
    }
    let now = current(tx, actor, collection, &push.name).await?;
    let merged = push
        .merge(now.as_ref().map(|(served, _, _)| served.as_str()))
        .map_err(|e| DomainError::Invalid(e.to_string()))?;
    let conflicts = merged.conflicts;
    match (merged.text, now) {
        // Nothing changes: no write, no new revision.
        (Some(text), Some((served, _, rev))) if text == served => {
            Ok(Pushed { name: push.name.clone(), revision: State(rev), body: Some(served), conflicts })
        }
        (None, None) => Ok(Pushed { name: push.name.clone(), revision: State(locked.sequence), body: None, conflicts }),
        (None, Some(_)) => {
            objects::delete_in(tx, locked, collection, &push.name, Precondition::None).await?;
            Ok(Pushed { name: push.name.clone(), revision: State(locked.sequence), body: None, conflicts })
        }
        (Some(text), now) => {
            let mut p = objects::prepare(locked.kind, &locked.components, &text)?;
            if p.uid.is_empty() {
                p.uid = format!("name:{}", push.name);
            }
            if !locked.access.can_write() {
                // Only the actor's own layer may change (decision 8).
                let same = now.as_ref().is_some_and(|(_, shared, _)| *shared == p.shared);
                if !same {
                    return Err(DomainError::ReadOnly);
                }
            }
            let w = objects::store(tx, locked, actor, collection, &push.name, p, Precondition::None).await?;
            let (served, _, _) = current(tx, actor, collection, &push.name).await?.ok_or(DomainError::NotFound)?;
            Ok(Pushed { name: push.name.clone(), revision: State(w.meta.revision), body: Some(served), conflicts })
        }
    }
}
