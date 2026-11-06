//! The calendar web app's view of events (phase 2 step 2): occurrences for
//! the grid, expanded by the core in the viewer's zone with their own
//! calendar settings applied; event writes through the core's lossless
//! editor (decision 25) with the recurring scope (decision 12); and undo.
//!
//! Every write runs in one transaction under the collection lock: the
//! `If-Match` check (a stale browser never overwrites a phone's edit), the
//! write, the change log, and an `object_history` row recording what it
//! replaced. Undo restores that, but only while nobody changed the object
//! since.

use std::collections::{BTreeMap, HashMap};

use calzone_core::edit::{self, EditError, EventChange, Scope};
use calzone_core::lines::Document;
use calzone_core::moment::{Key, Moment};
use calzone_core::recur::{self, When};
use calzone_core::split::UserLayer;
use chrono::{NaiveDate, NaiveDateTime, TimeZone};
use serde::Serialize;
use sqlx::types::Json;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

use super::collections::{self, CollectionKind};
use super::objects::{self, Precondition, delete_in, lock, prepare, served_body, store};
use super::prefs::{self, CalendarPrefs};
use super::{Actor, DomainError, Result};

/// The default colour for a calendar that has none.
pub const DEFAULT_COLOR: &str = "#3a7bd5";
const HISTORY_DAYS: i32 = 30;

/// One occurrence as the grid shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GridEvent {
    /// `<collection>/<name>`, plus `/<instance>` for a recurring one.
    pub id: String,
    pub collection: Uuid,
    pub name: String,
    pub etag: String,
    /// The instance key of a recurring event (`20260105T090000`).
    pub instance: Option<String>,
    pub title: String,
    pub location: Option<String>,
    /// Viewer-local `2026-01-05T09:00:00`, or `2026-01-05` when all day.
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub color: String,
    pub calendar: String,
    /// The owner's display name, for initials on calendars you do not own.
    pub owner: String,
    pub own: bool,
    pub editable: bool,
    pub recurring: bool,
}

fn key_of(w: &When) -> Key {
    match *w {
        When::Date(d) => Key::Date(d),
        When::Utc(t) | When::Floating(t) => Key::Time(t),
        When::Zoned { local, .. } => Key::Time(local),
    }
}

/// A `When` as viewer-local text for the grid.
fn local_text(w: &When, viewer: chrono_tz::Tz) -> String {
    match w {
        When::Date(d) => d.format("%Y-%m-%d").to_string(),
        other => viewer.from_utc_datetime(&other.instant(viewer)).naive_local().format("%Y-%m-%dT%H:%M:%S").to_string(),
    }
}

/// Every visible calendar's occurrences overlapping `[from, to)` (UTC),
/// in the viewer's zone. Hidden calendars (the viewer's own setting) are
/// left out; tasks are not shown.
pub async fn occurrences(
    pool: &PgPool,
    actor: Actor,
    from: NaiveDateTime,
    to: NaiveDateTime,
    viewer: chrono_tz::Tz,
) -> Result<Vec<GridEvent>> {
    let prefs = prefs::calendar_prefs(pool, actor).await?;
    // Owners by their display name, for initials ("Wendy Wife", not "wife").
    let labels: HashMap<Uuid, String> =
        super::accounts::list(pool).await?.into_iter().map(|a| (a.id, a.label().to_string())).collect();
    let mut out = Vec::new();
    for c in collections::visible(pool, actor, CollectionKind::Calendar).await? {
        let owner = labels.get(&c.owner_id).cloned().unwrap_or_else(|| c.owner_name.clone());
        let p = prefs.get(&c.id).cloned().unwrap_or_default();
        if p.hidden {
            continue;
        }
        let color = p.color.clone().or(c.color.clone()).unwrap_or_else(|| DEFAULT_COLOR.into());
        let calendar = p
            .display_name
            .clone()
            .unwrap_or_else(|| if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() });
        let names: Vec<String> =
            objects::in_range(pool, actor, c.id, from, to).await?.into_iter().map(|m| m.name).collect();
        for s in objects::get_many(pool, actor, c.id, &names).await? {
            if s.meta.kind != "VEVENT" {
                continue;
            }
            let Ok(exp) = recur::expand(&s.body, from, to, viewer) else { continue };
            for o in exp.occurrences {
                let instance = o.recurrence_id.as_ref().map(|r| key_of(r).to_string());
                let mut id = format!("{}/{}", c.id, s.meta.name);
                if let Some(k) = &instance {
                    id = format!("{id}/{k}");
                }
                out.push(GridEvent {
                    id,
                    collection: c.id,
                    name: s.meta.name.clone(),
                    etag: s.meta.etag(),
                    recurring: instance.is_some(),
                    instance,
                    title: o.summary.clone().unwrap_or_default(),
                    location: o.location.clone(),
                    start: local_text(&o.start, viewer),
                    end: local_text(&o.end, viewer),
                    all_day: matches!(o.start, When::Date(_)),
                    color: color.clone(),
                    calendar: calendar.clone(),
                    owner: owner.clone(),
                    own: c.owner_id == actor,
                    editable: c.access.can_write() && !c.subscribed,
                });
            }
        }
    }
    out.sort_by(|a, b| a.start.cmp(&b.start).then_with(|| a.title.cmp(&b.title)));
    Ok(out)
}

/// A time from the browser: a date, or a wall-clock time in the viewer's
/// zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInput {
    Date(NaiveDate),
    Local(NaiveDateTime),
}

impl std::str::FromStr for TimeInput {
    type Err = ();
    /// `2026-01-05`, `2026-01-05T09:00` or `2026-01-05T09:00:00`.
    fn from_str(s: &str) -> std::result::Result<Self, ()> {
        if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            return Ok(TimeInput::Date(d));
        }
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
            .map(TimeInput::Local)
            .map_err(|_| ())
    }
}

/// What the browser asks to change, times still in the viewer's terms.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventInput {
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Option<TimeInput>,
    pub end: Option<TimeInput>,
    pub rrule: Option<Option<String>>,
    pub alarms: Option<Vec<i64>>,
}

fn viewer_to_utc(viewer: chrono_tz::Tz, local: NaiveDateTime) -> NaiveDateTime {
    viewer.from_local_datetime(&local).earliest().map(|t| t.naive_utc()).unwrap_or(local)
}

/// A viewer-local time as a moment of the same kind as the event's own:
/// floating stays floating, UTC stays UTC, a zoned event keeps its zone
/// (converted, so 10:00 for the viewer is 10:00 for the viewer). New
/// events and all-day events turned timed take the viewer's zone.
pub fn to_moment(input: TimeInput, like: Option<&Moment>, viewer: chrono_tz::Tz) -> Moment {
    let local = match input {
        TimeInput::Date(d) => return Moment::Date(d),
        TimeInput::Local(l) => l,
    };
    match like {
        Some(Moment::Floating(_)) => Moment::Floating(local),
        Some(Moment::Utc(_)) => Moment::Utc(viewer_to_utc(viewer, local)),
        Some(Moment::Zoned { tzid, .. }) => match tzid.parse::<chrono_tz::Tz>() {
            Ok(tz) => Moment::Zoned {
                local: tz.from_utc_datetime(&viewer_to_utc(viewer, local)).naive_local(),
                tzid: tzid.clone(),
            },
            Err(_) => Moment::Zoned { local, tzid: viewer.name().to_string() },
        },
        _ => Moment::Zoned { local, tzid: viewer.name().to_string() },
    }
}

fn change_from(
    input: &EventInput,
    like_start: Option<&Moment>,
    like_end: Option<&Moment>,
    viewer: chrono_tz::Tz,
) -> EventChange {
    EventChange {
        summary: input.summary.clone(),
        location: input.location.clone(),
        description: input.description.clone(),
        start: input.start.map(|t| to_moment(t, like_start, viewer)),
        end: input.end.map(|t| to_moment(t, like_end.or(like_start), viewer)),
        rrule: input.rrule.clone(),
        alarms: input.alarms.clone(),
    }
}

fn edit_error(e: EditError) -> DomainError {
    match e {
        EditError::NoSuchInstance => DomainError::NotFound,
        other => DomainError::Invalid(other.to_string()),
    }
}

/// What a web write did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Saved {
    /// For undo.
    pub change_id: Uuid,
    /// The object's name and new ETag; `None` when it was deleted.
    pub name: String,
    pub etag: Option<String>,
    /// A new object a split made.
    pub created: Option<String>,
}

/// The object as it is before a write, for the history.
struct Before {
    body: Option<String>,
    layers: HashMap<String, UserLayer>,
}

async fn before(tx: &mut Transaction<'_, Postgres>, collection: Uuid, name: &str) -> Result<Before> {
    let row: Option<(Uuid, String)> =
        sqlx::query_as("SELECT id, body FROM objects WHERE collection_id = $1 AND name = $2")
            .bind(collection)
            .bind(name)
            .fetch_optional(&mut **tx)
            .await?;
    let Some((id, body)) = row else { return Ok(Before { body: None, layers: HashMap::new() }) };
    let layers: Vec<(Uuid, Json<UserLayer>)> =
        sqlx::query_as("SELECT principal_id, layer FROM user_layers WHERE object_id = $1")
            .bind(id)
            .fetch_all(&mut **tx)
            .await?;
    Ok(Before { body: Some(body), layers: layers.into_iter().map(|(p, Json(l))| (p.to_string(), l)).collect() })
}

async fn record(
    tx: &mut Transaction<'_, Postgres>,
    change_id: Uuid,
    actor: Actor,
    collection: Uuid,
    name: &str,
    before: Before,
    after_revision: Option<i64>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO object_history (change_id, principal_id, collection_id, name, before_body, before_layers, after_revision)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(change_id)
    .bind(actor)
    .bind(collection)
    .bind(name)
    .bind(before.body)
    .bind(Json(before.layers))
    .bind(after_revision)
    .execute(&mut **tx)
    .await?;
    sqlx::query("DELETE FROM object_history WHERE created_at < now() - make_interval(days => $1)")
        .bind(HISTORY_DAYS)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// The actor's view of an object inside the transaction: their layer merged.
async fn served_in(
    tx: &mut Transaction<'_, Postgres>,
    actor: Actor,
    collection: Uuid,
    name: &str,
) -> Result<(String, i64)> {
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
    let (body, rev, layer) = row.ok_or(DomainError::NotFound)?;
    Ok((served_body(body, layer), rev))
}

fn start_end_of(text: &str, instance: Option<Key>) -> (Option<Moment>, Option<Moment>) {
    let Ok(doc) = Document::parse(text) else { return (None, None) };
    let Some(cal) = doc.components.first() else { return (None, None) };
    let events: Vec<_> = cal.components().filter(|c| c.name == "VEVENT").collect();
    // The override for this instance if there is one, else the master.
    let pick = instance
        .and_then(|k| {
            events
                .iter()
                .find(|c| c.property("RECURRENCE-ID").and_then(Moment::from_line).is_some_and(|r| r.key() == k))
        })
        .or_else(|| events.iter().find(|c| c.property("RECURRENCE-ID").is_none()))
        .or(events.first());
    let Some(c) = pick else { return (None, None) };
    (c.property("DTSTART").and_then(Moment::from_line), c.property("DTEND").and_then(Moment::from_line))
}

/// Create an event in a calendar. The actor's default reminder for that
/// calendar applies when no reminders are given.
pub async fn create(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    input: &EventInput,
    viewer: chrono_tz::Tz,
    now: NaiveDateTime,
) -> Result<Saved> {
    let mut change = change_from(input, None, None, viewer);
    if change.alarms.is_none() {
        let p: CalendarPrefs = prefs::calendar_prefs(pool, actor).await?.remove(&collection).unwrap_or_default();
        change.alarms = p.default_alarm.map(|m| vec![i64::from(m)]);
    }
    let uid = Uuid::new_v4().to_string();
    let text = edit::new_event(&change, &uid, now).map_err(edit_error)?;
    create_object(pool, actor, collection, &format!("{uid}.ics"), &text).await
}

/// Store a new object (an event or a card) with a history row, so it can
/// be undone.
pub(crate) async fn create_object(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    text: &str,
) -> Result<Saved> {
    let name = name.to_string();
    let mut tx = pool.begin().await?;
    let mut locked = lock(&mut tx, actor, collection).await?;
    if !locked.access.can_write() {
        return Err(DomainError::ReadOnly);
    }
    let p = prepare(locked.kind, &locked.components, text)?;
    let w = store(&mut tx, &mut locked, actor, collection, &name, p, Precondition::IfNoneMatchAny).await?;
    let change_id = Uuid::new_v4();
    record(
        &mut tx,
        change_id,
        actor,
        collection,
        &name,
        Before { body: None, layers: HashMap::new() },
        Some(w.meta.revision),
    )
    .await?;
    tx.commit().await?;
    Ok(Saved { change_id, name, etag: Some(w.meta.etag()), created: None })
}

/// Change an event, or one or more occurrences of a recurring one. `etag`
/// is what the browser last saw: anything newer is refused
/// (`PreconditionFailed`), never overwritten.
#[allow(clippy::too_many_arguments)]
pub async fn update(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    etag: &str,
    instance: Option<Key>,
    scope: Scope,
    input: &EventInput,
    viewer: chrono_tz::Tz,
    now: NaiveDateTime,
) -> Result<Saved> {
    write(pool, actor, collection, name, etag, |text| {
        let (s, e) = start_end_of(text, instance);
        let change = change_from(input, s.as_ref(), e.as_ref(), viewer);
        edit::edit(text, instance, scope, &change, now, &Uuid::new_v4().to_string()).map_err(edit_error)
    })
    .await
}

/// Delete an event, one occurrence, or an occurrence and the rest.
#[allow(clippy::too_many_arguments)]
pub async fn remove(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    etag: &str,
    instance: Option<Key>,
    scope: Scope,
    now: NaiveDateTime,
) -> Result<Saved> {
    write(pool, actor, collection, name, etag, |text| edit::delete(text, instance, scope, now).map_err(edit_error))
        .await
}

/// Change or delete an object (an event or a card) as the actor sees it,
/// under its ETag, with a history row. `apply` gets the actor's view and
/// returns the new text (or `None` to delete, or a second object).
pub(crate) async fn write(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    etag: &str,
    apply: impl FnOnce(&str) -> Result<edit::Outcome>,
) -> Result<Saved> {
    let mut tx = pool.begin().await?;
    let mut locked = lock(&mut tx, actor, collection).await?;
    if !locked.access.can_write() {
        return Err(DomainError::ReadOnly);
    }
    let (text, rev) = served_in(&mut tx, actor, collection, name).await?;
    if objects::etag(rev) != etag {
        return Err(DomainError::PreconditionFailed);
    }
    let outcome = apply(&text)?;
    let change_id = Uuid::new_v4();
    let prior = before(&mut tx, collection, name).await?;
    let others: Vec<(String, UserLayer)> =
        prior.layers.iter().filter(|(p, _)| **p != actor.to_string()).map(|(p, l)| (p.clone(), l.clone())).collect();
    let etag_after = match &outcome.updated {
        Some(updated) => {
            let p = prepare(locked.kind, &locked.components, updated)?;
            let w = store(&mut tx, &mut locked, actor, collection, name, p, Precondition::IfMatch(etag.to_string()))
                .await?;
            record(&mut tx, change_id, actor, collection, name, prior, Some(w.meta.revision)).await?;
            Some(w.meta.etag())
        }
        None => {
            delete_in(&mut tx, &mut locked, collection, name, Precondition::IfMatch(etag.to_string())).await?;
            record(&mut tx, change_id, actor, collection, name, prior, None).await?;
            None
        }
    };
    let mut created = None;
    if let Some(new) = &outcome.created {
        let p = prepare(locked.kind, &locked.components, new)?;
        let new_uid = p_uid(new);
        let new_name = format!("{new_uid}.ics");
        let w = store(&mut tx, &mut locked, actor, collection, &new_name, p, Precondition::IfNoneMatchAny).await?;
        carry_reminders(&mut tx, collection, &new_name, &new_uid, &others).await?;
        record(
            &mut tx,
            change_id,
            actor,
            collection,
            &new_name,
            Before { body: None, layers: HashMap::new() },
            Some(w.meta.revision),
        )
        .await?;
        created = Some(new_name);
    }
    tx.commit().await?;
    Ok(Saved { change_id, name: name.to_string(), etag: etag_after, created })
}

/// A split's new series keeps everyone else's series-wide reminders (the
/// editor's own travel in the text). Reminders and snoozes for single
/// occurrences stay with the old series, where those occurrences are.
async fn carry_reminders(
    tx: &mut Transaction<'_, Postgres>,
    collection: Uuid,
    name: &str,
    uid: &str,
    others: &[(String, UserLayer)],
) -> Result<()> {
    let id: Uuid = sqlx::query_scalar("SELECT id FROM objects WHERE collection_id = $1 AND name = $2")
        .bind(collection)
        .bind(name)
        .fetch_one(&mut **tx)
        .await?;
    for (who, layer) in others {
        let Ok(who) = who.parse::<Uuid>() else { continue };
        let entries: Vec<_> = layer
            .entries
            .iter()
            .filter(|e| {
                e.recurrence_id.is_none() && e.source.trim_start().to_ascii_uppercase().starts_with("BEGIN:VALARM")
            })
            .map(|e| calzone_core::split::LayerEntry { uid: uid.to_string(), ..e.clone() })
            .collect();
        if entries.is_empty() {
            continue;
        }
        sqlx::query(
            "INSERT INTO user_layers (object_id, principal_id, layer) VALUES ($1, $2, $3)
             ON CONFLICT (object_id, principal_id) DO UPDATE SET layer = EXCLUDED.layer",
        )
        .bind(id)
        .bind(who)
        .bind(Json(UserLayer { entries }))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

/// Set the actor's own reminders on an event (every occurrence) and
/// nothing else. Allowed with view access too: the shared object must come
/// out byte for byte the same, so only the actor's layer changes (decision
/// 8). Recorded in the history like any write, so it can be undone.
pub async fn update_reminders(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    etag: &str,
    minutes: &[i64],
) -> Result<Saved> {
    let mut tx = pool.begin().await?;
    let mut locked = lock(&mut tx, actor, collection).await?;
    let (text, rev) = served_in(&mut tx, actor, collection, name).await?;
    if objects::etag(rev) != etag {
        return Err(DomainError::PreconditionFailed);
    }
    let updated = edit::set_alarms(&text, minutes).map_err(edit_error)?;
    let prior = before(&mut tx, collection, name).await?;
    let p = prepare(locked.kind, &locked.components, &updated)?;
    if prior.body.as_deref() != Some(p.shared.as_str()) {
        // Only the layer may change through here; anything else is an edit.
        return Err(DomainError::ReadOnly);
    }
    let w = store(&mut tx, &mut locked, actor, collection, name, p, Precondition::IfMatch(etag.to_string())).await?;
    let change_id = Uuid::new_v4();
    record(&mut tx, change_id, actor, collection, name, prior, Some(w.meta.revision)).await?;
    tx.commit().await?;
    Ok(Saved { change_id, name: name.to_string(), etag: Some(w.meta.etag()), created: None })
}

/// The UID of a new object the editor made (always a fresh UUID).
fn p_uid(text: &str) -> String {
    Document::parse(text)
        .ok()
        .and_then(|d| {
            d.components
                .first()?
                .components()
                .find(|c| c.name == "VEVENT")?
                .property("UID")
                .map(|l| l.value().to_string())
        })
        .unwrap_or_else(|| Uuid::new_v4().to_string())
}

/// Undo a change the actor made in the last 30 days: every object it
/// touched goes back to what it was, layers included. Refused
/// (`PreconditionFailed`) when any of them changed since; `NotFound` when
/// there is nothing (left) to undo.
pub async fn undo(pool: &PgPool, actor: Actor, change_id: Uuid) -> Result<()> {
    type Row = (i64, Uuid, String, Option<String>, Json<HashMap<String, UserLayer>>, Option<i64>);
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, collection_id, name, before_body, before_layers, after_revision FROM object_history
         WHERE change_id = $1 AND principal_id = $2 AND undone_at IS NULL
           AND created_at > now() - make_interval(days => $3)
         ORDER BY id",
    )
    .bind(change_id)
    .bind(actor)
    .bind(HISTORY_DAYS)
    .fetch_all(pool)
    .await?;
    if rows.is_empty() {
        return Err(DomainError::NotFound);
    }
    let mut tx = pool.begin().await?;
    // Lock every collection involved, in a fixed order.
    let mut locks = BTreeMap::new();
    for (_, c, ..) in &rows {
        if !locks.contains_key(c) {
            let l = lock(&mut tx, actor, *c).await?;
            locks.insert(*c, l);
        }
    }
    for (_, c, name, before_body, _, after) in &rows {
        let now: Option<(i64, String)> =
            sqlx::query_as("SELECT revision, body FROM objects WHERE collection_id = $1 AND name = $2")
                .bind(c)
                .bind(name)
                .fetch_optional(&mut *tx)
                .await?;
        if now.as_ref().map(|n| n.0) != *after {
            return Err(DomainError::PreconditionFailed);
        }
        // Without write access only a layer-only change can be undone (a
        // reader's own reminders): the shared object must stay the same.
        if !locks[c].access.can_write() && now.map(|n| n.1) != *before_body {
            return Err(DomainError::ReadOnly);
        }
    }
    for (_, c, name, body, Json(layers), after) in rows.iter().rev() {
        let locked = locks.get_mut(c).expect("locked above");
        match body {
            None => delete_in(&mut tx, locked, *c, name, Precondition::None).await?,
            Some(body) => {
                let p = prepare(locked.kind, &locked.components, body)?;
                let pre = if after.is_some() { Precondition::None } else { Precondition::IfNoneMatchAny };
                store(&mut tx, locked, actor, *c, name, p, pre).await?;
                let id: Uuid = sqlx::query_scalar("SELECT id FROM objects WHERE collection_id = $1 AND name = $2")
                    .bind(c)
                    .bind(name)
                    .fetch_one(&mut *tx)
                    .await?;
                sqlx::query("DELETE FROM user_layers WHERE object_id = $1").bind(id).execute(&mut *tx).await?;
                for (who, layer) in layers {
                    let Ok(who) = who.parse::<Uuid>() else { continue };
                    sqlx::query(
                        "INSERT INTO user_layers (object_id, principal_id, layer)
                         SELECT $1, $2, $3 WHERE EXISTS (SELECT 1 FROM principals WHERE id = $2)",
                    )
                    .bind(id)
                    .bind(who)
                    .bind(Json(layer))
                    .execute(&mut *tx)
                    .await?;
                }
            }
        }
    }
    sqlx::query("UPDATE object_history SET undone_at = now() WHERE change_id = $1")
        .bind(change_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// One event (or occurrence) as the panel shows it, in the viewer's own
/// wall-clock time, with only their own reminders.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventDetail {
    pub collection: Uuid,
    pub name: String,
    pub etag: String,
    pub instance: Option<String>,
    pub title: String,
    pub location: String,
    pub description: String,
    pub all_day: bool,
    pub start_date: String,
    pub start_time: String,
    /// For all-day events the last day (inclusive), as people say it.
    pub end_date: String,
    pub end_time: String,
    /// `none`, `daily`, `weekly`, `monthly`, `yearly`, or `custom` for any
    /// other rule (kept as it is unless changed).
    pub repeat: &'static str,
    pub recurring: bool,
    /// Minutes before the start; `None` when a reminder is not one the
    /// panel offers (it is kept as it is unless changed).
    pub alarm: Option<i64>,
    pub alarm_kept: bool,
    pub calendar: String,
    pub owner: String,
    pub own: bool,
    pub editable: bool,
    pub color: String,
}

/// A moment as viewer-local (date, time, all day).
fn local_parts(m: &Moment, doc: &Document, viewer: chrono_tz::Tz) -> (NaiveDate, Option<chrono::NaiveTime>) {
    let utc_to_local = |u: NaiveDateTime| viewer.from_utc_datetime(&u).naive_local();
    let local = match m {
        Moment::Date(d) => return (*d, None),
        Moment::Floating(t) => *t,
        Moment::Utc(t) => utc_to_local(*t),
        Moment::Zoned { local, tzid } => utc_to_local(calzone_core::moment::zoned_to_utc(*local, tzid, doc)),
    };
    (local.date(), Some(local.time()))
}

fn repeat_of(rule: Option<&str>) -> &'static str {
    match rule.map(|r| r.trim().to_ascii_uppercase()).as_deref() {
        None => "none",
        Some("FREQ=DAILY") => "daily",
        Some("FREQ=WEEKLY") => "weekly",
        Some("FREQ=MONTHLY") => "monthly",
        Some("FREQ=YEARLY") => "yearly",
        Some(_) => "custom",
    }
}

/// The panel's reminder presets, in minutes before.
pub const ALARM_PRESETS: [i64; 6] = [0, 5, 15, 30, 60, 1440];

pub async fn detail(
    pool: &PgPool,
    actor: Actor,
    collection: Uuid,
    name: &str,
    instance: Option<Key>,
    viewer: chrono_tz::Tz,
) -> Result<EventDetail> {
    let c = collections::get(pool, actor, collection).await?;
    let served = objects::get(pool, actor, collection, name).await?;
    let doc = Document::parse(&served.body).map_err(|_| DomainError::NotFound)?;
    let cal = doc.components.first().ok_or(DomainError::NotFound)?;
    let events: Vec<_> = cal.components().filter(|x| x.name == "VEVENT").collect();
    let master = events
        .iter()
        .copied()
        .find(|x| x.property("RECURRENCE-ID").is_none())
        .or(events.first().copied())
        .ok_or(DomainError::NotFound)?;
    let master_start = master.property("DTSTART").and_then(Moment::from_line).ok_or(DomainError::NotFound)?;
    let master_end = master.property("DTEND").and_then(Moment::from_line).unwrap_or_else(|| {
        let d = master.property("DURATION").and_then(|l| edit::parse_duration(l.value()));
        match d {
            Some(d) => master_start.shifted(d),
            None if master_start.is_date() => master_start.shifted(chrono::Duration::days(1)),
            None => master_start.clone(),
        }
    });
    let recurring = master.property("RRULE").is_some() || master.property("RDATE").is_some();
    let over = instance.and_then(|k| {
        events
            .iter()
            .copied()
            .find(|x| x.property("RECURRENCE-ID").and_then(Moment::from_line).is_some_and(|r| r.key() == k))
    });
    let part = over.unwrap_or(master);
    let (start, end) = match (over, instance) {
        (Some(o), _) => {
            let s = o.property("DTSTART").and_then(Moment::from_line).unwrap_or_else(|| master_start.clone());
            let e = o.property("DTEND").and_then(Moment::from_line).unwrap_or_else(|| s.clone());
            (s, e)
        }
        (None, Some(k)) if recurring => {
            let s = master_start.with_key(k);
            let len = match (master_start.key(), master_end.key()) {
                (Key::Date(a), Key::Date(b)) => b - a,
                (Key::Time(a), Key::Time(b)) => b - a,
                _ => chrono::Duration::zero(),
            };
            (s.clone(), s.shifted(len))
        }
        _ => (master_start.clone(), master_end.clone()),
    };
    let (sd, st) = local_parts(&start, &doc, viewer);
    let (mut ed, et) = local_parts(&end, &doc, viewer);
    let all_day = st.is_none();
    if all_day {
        // Stored exclusive; shown as the last day.
        ed = (ed - chrono::Duration::days(1)).max(sd);
    }
    let text = |n: &str| part.property(n).map(|l| calzone_core::object::unescape(l.value())).unwrap_or_default();
    let alarms: Vec<i64> = part
        .components()
        .filter(|a| a.name == "VALARM")
        .filter_map(|a| a.property("TRIGGER"))
        .filter(|t| t.param("VALUE").is_none_or(|v| !v.eq_ignore_ascii_case("DATE-TIME")))
        .filter_map(|t| edit::parse_duration(t.value()))
        .map(|d| -d.num_minutes())
        .collect();
    let (alarm, alarm_kept) = match alarms.as_slice() {
        [] => (None, false),
        [m] if ALARM_PRESETS.contains(m) => (Some(*m), false),
        _ => (None, true),
    };
    let prefs = prefs::calendar_prefs(pool, actor).await?.remove(&collection).unwrap_or_default();
    let fmt_t = |t: Option<chrono::NaiveTime>| t.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
    Ok(EventDetail {
        collection,
        name: name.to_string(),
        etag: served.meta.etag(),
        instance: instance.filter(|_| recurring || over.is_some()).map(|k| k.to_string()),
        title: text("SUMMARY"),
        location: text("LOCATION"),
        description: text("DESCRIPTION"),
        all_day,
        start_date: sd.format("%Y-%m-%d").to_string(),
        start_time: fmt_t(st),
        end_date: ed.format("%Y-%m-%d").to_string(),
        end_time: fmt_t(et),
        repeat: repeat_of(master.property("RRULE").map(|l| l.value())),
        recurring,
        alarm,
        alarm_kept,
        calendar: prefs
            .display_name
            .unwrap_or_else(|| if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() }),
        owner: super::accounts::get(pool, c.owner_id)
            .await
            .map(|a| a.label().to_string())
            .unwrap_or_else(|_| c.owner_name.clone()),
        own: c.owner_id == actor,
        editable: c.access.can_write() && !c.subscribed,
        color: prefs.color.or(c.color).unwrap_or_else(|| DEFAULT_COLOR.into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn browser_times() {
        assert_eq!("2026-01-05".parse(), Ok(TimeInput::Date(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap())));
        assert_eq!("2026-01-05T09:30".parse(), Ok(TimeInput::Local(t("2026-01-05 09:30"))));
        assert_eq!("2026-01-05T09:30:00".parse(), Ok(TimeInput::Local(t("2026-01-05 09:30"))));
        assert!("monday".parse::<TimeInput>().is_err());
    }

    #[test]
    fn times_keep_the_events_kind() {
        let ams = chrono_tz::Europe::Amsterdam;
        let local = TimeInput::Local(t("2026-01-05 10:00"));
        assert_eq!(
            to_moment(local, Some(&Moment::Floating(t("2026-01-01 00:00"))), ams),
            Moment::Floating(t("2026-01-05 10:00"))
        );
        assert_eq!(
            to_moment(local, Some(&Moment::Utc(t("2026-01-01 00:00"))), ams),
            Moment::Utc(t("2026-01-05 09:00"))
        );
        let ny = Moment::Zoned { local: t("2026-01-01 00:00"), tzid: "America/New_York".into() };
        assert_eq!(
            to_moment(local, Some(&ny), ams),
            Moment::Zoned { local: t("2026-01-05 04:00"), tzid: "America/New_York".into() }
        );
        assert_eq!(
            to_moment(local, None, ams),
            Moment::Zoned { local: t("2026-01-05 10:00"), tzid: "Europe/Amsterdam".into() }
        );
        let day = TimeInput::Date(NaiveDate::from_ymd_opt(2026, 1, 5).unwrap());
        assert!(to_moment(day, Some(&ny), ams).is_date());
    }
}
