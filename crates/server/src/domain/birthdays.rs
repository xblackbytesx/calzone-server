//! Birthdays from contacts (decision 57): each person's own Birthdays
//! calendar, kept by the server from the address books they read and the
//! ones shared with them for birthdays only.
//!
//! - **Stored, read-only, never shared.** An ordinary calendar with a
//!   `birthday_calendars` row: the object lock downgrades everyone to read
//!   (as for subscriptions), and it cannot get grants or feed links,
//!   because it holds names and dates others shared with this person only.
//!   So phones, push and the DAV door serve it like any calendar.
//! - **Rebuilt when its sources change.** [`rebuild`] compares what it was
//!   built from (each source book's sequence, the settings, the language)
//!   with now, and writes only the events that changed, under UIDs that
//!   stay the same. The background task checks every few seconds.
//! - **Birthdays-only sharing** is its own table (`birthday_shares`) that
//!   no access check reads: the person gets names and dates in their
//!   Birthdays, never the cards, never a link to them.
//! - **Reminders** are the person's own (decision 8): one setting for every
//!   event, kept in their layer; refreshes never touch layers.

use std::collections::{HashMap, HashSet};

use calzone_core::birthdays::{self, Day, Entry, Occasion, Person};
use calzone_core::card::{self, CardChange};
use calzone_core::edit;
use sqlx::PgPool;
use uuid::Uuid;

use super::access;
use super::events::{self, Target};
use super::objects::{Precondition, delete_in, lock_for_refresh, prepare, store};
use super::{Actor, CollectionKind, DomainError, NewCollection, Result, collections};

/// Reminders people can pick, minutes before the day starts: 9:00 on the
/// day, 9:00 the day before, 9:00 a week before.
pub const REMINDERS: [i32; 3] = [-540, 900, 9540];

/// A person's Birthdays settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub collection: Uuid,
    pub anniversaries: bool,
    pub reminder: Option<i32>,
}

/// The person's Birthdays calendar, when it is on.
pub async fn of(pool: &PgPool, actor: Actor) -> Result<Option<Settings>> {
    let row: Option<(Uuid, bool, Option<i32>)> =
        sqlx::query_as("SELECT collection_id, anniversaries, reminder FROM birthday_calendars WHERE principal_id = $1")
            .bind(actor)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(collection, anniversaries, reminder)| Settings { collection, anniversaries, reminder }))
}

/// Turn Birthdays on: a calendar named `name` (in the person's language,
/// `lang`), filled at once. Already on: the one there is.
pub async fn enable(pool: &PgPool, actor: Actor, name: &str, lang: &str) -> Result<Uuid> {
    if let Some(s) = of(pool, actor).await? {
        return Ok(s.collection);
    }
    let lang = if lang == "nl" { "nl" } else { "en" };
    let new = NewCollection {
        kind: CollectionKind::Calendar,
        slug: "",
        display_name: name,
        description: "",
        color: Some("#d9a400"),
        components: &["VEVENT"],
    };
    let c = collections::create_numbered(pool, actor, "birthdays", new).await?;
    let made = sqlx::query(
        "INSERT INTO birthday_calendars (collection_id, principal_id, language) VALUES ($1, $2, $3)
         ON CONFLICT (principal_id) DO NOTHING",
    )
    .bind(c.id)
    .bind(actor)
    .bind(lang)
    .execute(pool)
    .await?
    .rows_affected();
    if made == 0 {
        // Turned on twice at once: keep the first.
        sqlx::query("DELETE FROM collections WHERE id = $1").bind(c.id).execute(pool).await?;
        return of(pool, actor).await?.map(|s| s.collection).ok_or(DomainError::NotFound);
    }
    rebuild(pool, c.id).await?;
    Ok(c.id)
}

/// Turn Birthdays off: the calendar goes from every device. Contacts and
/// the person's choices of books stay.
pub async fn disable(pool: &PgPool, actor: Actor) -> Result<()> {
    if let Some(s) = of(pool, actor).await? {
        sqlx::query("DELETE FROM collections WHERE id = $1").bind(s.collection).execute(pool).await?;
    }
    Ok(())
}

/// An address book that can feed a person's Birthdays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Book {
    pub id: Uuid,
    pub name: String,
    pub owner_id: Uuid,
    pub owner: String,
    pub own: bool,
    /// Shared for birthdays only: no access to its cards.
    pub birthdays_only: bool,
    pub included: bool,
    pub sequence: i64,
}

/// Every address book the person reads or got birthdays from, own first.
pub async fn books(pool: &PgPool, actor: Actor) -> Result<Vec<Book>> {
    let sql = format!(
        "SELECT * FROM (
            SELECT c.id, c.display_name, c.slug, c.owner_id, o.name, o.display_name, c.sequence, ({lvl}) AS lvl,
                   EXISTS (SELECT 1 FROM birthday_shares s WHERE s.collection_id = c.id
                            AND (s.principal_id = $1
                                 OR s.principal_id IN (SELECT m.group_id FROM memberships m WHERE m.member_id = $1))) AS shared,
                   EXISTS (SELECT 1 FROM birthday_skips k WHERE k.collection_id = c.id AND k.principal_id = $1) AS skipped
            FROM collections c JOIN principals o ON o.id = c.owner_id
            WHERE c.kind = 'addressbook'
         ) v
         WHERE (v.lvl IS NOT NULL OR v.shared)
           AND NOT EXISTS (SELECT 1 FROM principals p WHERE p.id = $1 AND p.disabled)
         ORDER BY v.lvl DESC NULLS LAST, v.name, v.slug",
        lvl = access::level_sql("$1")
    );
    type Row = (Uuid, String, String, Uuid, String, String, i64, Option<i32>, bool, bool);
    let rows: Vec<Row> = sqlx::query_as(&sql).bind(actor).fetch_all(pool).await?;
    Ok(rows
        .into_iter()
        .map(|(id, display, slug, owner_id, owner_name, owner_display, sequence, lvl, _, skipped)| Book {
            id,
            name: if display.is_empty() { slug } else { display },
            owner_id,
            owner: if owner_display.is_empty() { owner_name } else { owner_display },
            own: owner_id == actor,
            birthdays_only: lvl.is_none(),
            included: !skipped,
            sequence,
        })
        .collect())
}

/// Change the settings: anniversaries, the reminder on every event, and
/// which books are left out. Rebuilt at once.
pub async fn set_options(
    pool: &PgPool,
    actor: Actor,
    anniversaries: bool,
    reminder: Option<i32>,
    included: &[Uuid],
) -> Result<()> {
    let s = of(pool, actor).await?.ok_or(DomainError::NotFound)?;
    if reminder.is_some_and(|r| !REMINDERS.contains(&r)) {
        return Err(DomainError::Invalid("a reminder this page offers".into()));
    }
    let all = books(pool, actor).await?;
    let mut tx = pool.begin().await?;
    sqlx::query("UPDATE birthday_calendars SET anniversaries = $2, reminder = $3 WHERE collection_id = $1")
        .bind(s.collection)
        .bind(anniversaries)
        .bind(reminder)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM birthday_skips WHERE principal_id = $1").bind(actor).execute(&mut *tx).await?;
    for b in all.iter().filter(|b| !included.contains(&b.id)) {
        sqlx::query("INSERT INTO birthday_skips (principal_id, collection_id) VALUES ($1, $2)")
            .bind(actor)
            .bind(b.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    rebuild(pool, s.collection).await?;
    if reminder != s.reminder {
        set_reminders(pool, s.collection, actor, reminder).await?;
    }
    Ok(())
}

/// Who gets this address book's birthdays only (its admins may see).
pub async fn shares(pool: &PgPool, actor: Actor, book: Uuid) -> Result<Vec<Uuid>> {
    admin_book(pool, actor, book).await?;
    Ok(sqlx::query_scalar("SELECT principal_id FROM birthday_shares WHERE collection_id = $1 ORDER BY created_at")
        .bind(book)
        .fetch_all(pool)
        .await?)
}

/// Share an address book's birthdays (names and dates only) with a person
/// or group, or stop. Whoever may share the book may.
pub async fn set_share(pool: &PgPool, actor: Actor, book: Uuid, principal: Uuid, on: bool) -> Result<()> {
    let c = admin_book(pool, actor, book).await?;
    if on {
        if principal == c.owner_id {
            return Err(DomainError::Invalid("the owner has the whole address book".into()));
        }
        sqlx::query("INSERT INTO birthday_shares (collection_id, principal_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(book)
            .bind(principal)
            .execute(pool)
            .await?;
    } else {
        sqlx::query("DELETE FROM birthday_shares WHERE collection_id = $1 AND principal_id = $2")
            .bind(book)
            .bind(principal)
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn admin_book(pool: &PgPool, actor: Actor, book: Uuid) -> Result<super::Collection> {
    let c = collections::get(pool, actor, book).await?;
    if c.kind != CollectionKind::AddressBook {
        return Err(DomainError::NotFound);
    }
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly);
    }
    Ok(c)
}

/// The people on the cards of these books with a date: own books first,
/// then shared ones, then birthdays-only ones. Read here without an
/// actor: [`books`] decided what this person gets.
async fn people(pool: &PgPool, sources: &[&Book]) -> Result<Vec<(Person, Uuid, String)>> {
    let ids: Vec<Uuid> = sources.iter().map(|b| b.id).collect();
    let rows: Vec<(Uuid, String, String)> = sqlx::query_as(
        "SELECT collection_id, name, body FROM objects
         WHERE collection_id = ANY($1) AND kind = 'VCARD' AND body ~* '(BDAY|ANNIVERSARY|X-ABDATE)'
         ORDER BY collection_id, name",
    )
    .bind(&ids)
    .fetch_all(pool)
    .await?;
    let rank: HashMap<Uuid, u32> = sources
        .iter()
        .map(|b| {
            (
                b.id,
                if b.own {
                    0
                } else if !b.birthdays_only {
                    1
                } else {
                    2
                },
            )
        })
        .collect();
    Ok(rows
        .into_iter()
        .filter_map(|(c, name, body)| {
            let p = birthdays::person(&body, rank.get(&c).copied().unwrap_or(3), format!("{c}/{name}"))?;
            Some((p, c, name))
        })
        .collect())
}

fn title(e: &Entry, lang: &str) -> String {
    match (e.occasion, lang) {
        (Occasion::Birthday, _) => e.name.clone(),
        (Occasion::Anniversary, "nl") => format!("{}, trouwdag", e.name),
        (Occasion::Anniversary, _) => format!("{}, wedding anniversary", e.name),
    }
}

fn object_name(uid: &str) -> String {
    format!("{}.ics", uid.trim_end_matches("@calzone"))
}

/// Make the Birthdays calendar match its sources now. Nothing is written
/// when nothing changed since the last rebuild (`false`). Events that did
/// not change keep their revision, and everyone's own reminders stay.
pub async fn rebuild(pool: &PgPool, collection: Uuid) -> Result<bool> {
    /// person, anniversaries, reminder, fallback language, chosen language, built from, disabled.
    type Row = (Uuid, bool, Option<i32>, String, Option<String>, String, bool);
    let row: Option<Row> = sqlx::query_as(
        "SELECT b.principal_id, b.anniversaries, b.reminder, b.language, p.language, b.built_from, p.disabled
         FROM birthday_calendars b JOIN principals p ON p.id = b.principal_id WHERE b.collection_id = $1",
    )
    .bind(collection)
    .fetch_optional(pool)
    .await?;
    let Some((person, anniversaries, reminder, fallback, chosen, built_from, disabled)) = row else {
        return Err(DomainError::NotFound);
    };
    if disabled {
        return Ok(false);
    }
    let lang = chosen.filter(|l| l == "nl" || l == "en").unwrap_or(fallback);
    let all = books(pool, person).await?;
    let sources: Vec<&Book> = all.iter().filter(|b| b.included).collect();
    // What this rebuild is made of: read before the cards, so a change
    // made while it runs shows up as different next time.
    let mut from: Vec<String> = sources
        .iter()
        .map(|b| format!("{}:{}:{}:{}", b.id, b.sequence, u8::from(b.own), u8::from(b.birthdays_only)))
        .collect();
    from.sort();
    let from = format!("v1|{lang}|{}|{}", u8::from(anniversaries), from.join(","));
    if from == built_from {
        return Ok(false);
    }
    let found = people(pool, &sources).await?;
    let readable: HashSet<Uuid> = sources.iter().filter(|b| !b.birthdays_only).map(|b| b.id).collect();
    let list: Vec<Person> = found.iter().map(|(p, ..)| p.clone()).collect();
    let mut wanted: Vec<(String, String)> = Vec::new();
    for e in birthdays::entries(&list, anniversaries) {
        // Link a card this person may open: the shown one if it is, else
        // another card of the same person.
        let link = std::iter::once(e.person)
            .chain(e.cards.iter().copied())
            .find(|&i| readable.contains(&found[i].1))
            .map(|i| format!("{}/{}", found[i].1, found[i].2));
        wanted.push((e.uid.clone(), birthdays::event(&e, &title(&e, &lang), link.as_deref())));
    }

    let mut tx = pool.begin().await?;
    let mut locked = lock_for_refresh(&mut tx, collection).await?;
    let existing: Vec<(String, String, String)> =
        sqlx::query_as("SELECT name, uid, body FROM objects WHERE collection_id = $1")
            .bind(collection)
            .fetch_all(&mut *tx)
            .await?;
    let by_uid: HashMap<&str, (&str, &str)> =
        existing.iter().map(|(n, u, b)| (u.as_str(), (n.as_str(), b.as_str()))).collect();
    let mut seen = HashSet::new();
    for (uid, text) in &wanted {
        if !seen.insert(uid.clone()) {
            continue;
        }
        match by_uid.get(uid.as_str()) {
            Some((_, body)) if *body == text.as_str() => {}
            Some((name, _)) => {
                let name = name.to_string();
                let p = prepare(locked.kind, &locked.components, text)?.without_layer();
                store(&mut tx, &mut locked, person, collection, &name, p, Precondition::None).await?;
            }
            None => {
                // New: with the person's reminder, as their own layer.
                let text = match reminder {
                    Some(m) => {
                        edit::set_alarms(text, &[i64::from(m)]).map_err(|e| DomainError::Invalid(e.to_string()))?
                    }
                    None => text.clone(),
                };
                let p = prepare(locked.kind, &locked.components, &text)?;
                store(&mut tx, &mut locked, person, collection, &object_name(uid), p, Precondition::None).await?;
            }
        }
    }
    for (name, uid, _) in &existing {
        if !seen.contains(uid) {
            delete_in(&mut tx, &mut locked, collection, name, Precondition::None).await?;
        }
    }
    sqlx::query("UPDATE birthday_calendars SET built_from = $2 WHERE collection_id = $1")
        .bind(collection)
        .bind(&from)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(true)
}

/// Put the person's reminder on every event (or take it off), replacing
/// the reminders they had on them.
async fn set_reminders(pool: &PgPool, collection: Uuid, person: Uuid, reminder: Option<i32>) -> Result<()> {
    let mut tx = pool.begin().await?;
    let mut locked = lock_for_refresh(&mut tx, collection).await?;
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT name, body FROM objects WHERE collection_id = $1")
        .bind(collection)
        .fetch_all(&mut *tx)
        .await?;
    let minutes: Vec<i64> = reminder.map(i64::from).into_iter().collect();
    for (name, body) in rows {
        let text = edit::set_alarms(&body, &minutes).map_err(|e| DomainError::Invalid(e.to_string()))?;
        let p = prepare(locked.kind, &locked.components, &text)?;
        store(&mut tx, &mut locked, person, collection, &name, p, Precondition::None).await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Every Birthdays calendar whose sources changed since it was built,
/// rebuilt; how many were. For the background task.
pub async fn rebuild_changed(pool: &PgPool) -> Result<usize> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT b.collection_id FROM birthday_calendars b JOIN principals p ON p.id = b.principal_id
         WHERE NOT p.disabled ORDER BY b.collection_id",
    )
    .fetch_all(pool)
    .await?;
    let mut n = 0;
    for id in ids {
        match rebuild(pool, id).await {
            Ok(true) => n += 1,
            Ok(false) | Err(DomainError::NotFound) => {}
            Err(e) => tracing::warn!(error = %e, "birthdays: a rebuild failed"),
        }
    }
    Ok(n)
}

/// Rebuild every few seconds what changed. Cheap when nothing did: one
/// query per Birthdays calendar.
pub fn spawn(pool: PgPool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(10));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            if let Err(e) = rebuild_changed(&pool).await {
                tracing::warn!(error = %e, "birthdays: could not check for changes");
            }
        }
    });
}

// ---- Manual birthday events: found, and removed in one change ----

/// What a manual yearly event is to the person's contacts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Found {
    /// A contact has this birthday: Birthdays shows it already.
    Covered { contact: String },
    /// A contact the person may edit has no birthday yet: copy the date
    /// to it first.
    Copy { contact: String, book: Uuid, name: String, etag: String },
    /// A contact has a different birthday: one of the two is wrong.
    Differs { contact: String, birthday: Day },
}

/// One manual birthday event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManualEvent {
    pub collection: Uuid,
    pub calendar: String,
    /// Others can see or edit that calendar too.
    pub shared: bool,
    pub name: String,
    pub etag: String,
    pub title: String,
    pub start: chrono::NaiveDate,
    pub found: Found,
}

/// The yearly all-day events in calendars the person may edit that name
/// one of their contacts (the ones their Birthdays reads). Needs Birthdays
/// on: removing them is only safe with it.
pub async fn manual(pool: &PgPool, actor: Actor) -> Result<Vec<ManualEvent>> {
    use chrono::Datelike;
    of(pool, actor).await?.ok_or(DomainError::NotFound)?;
    let all = books(pool, actor).await?;
    let sources: Vec<&Book> = all.iter().filter(|b| b.included).collect();
    let found = people(pool, &sources).await?;
    // Cards without a date too, for "copy the date": the person's writable books.
    let writable: Vec<super::Collection> = collections::visible(pool, actor, CollectionKind::AddressBook)
        .await?
        .into_iter()
        .filter(|c| c.access.can_write())
        .collect();
    let mut plain: Vec<(card::CardView, Uuid, String, String)> = Vec::new();
    for b in &writable {
        for s in super::objects::all(pool, actor, b.id).await? {
            if let Ok(v) = card::read(&s.body)
                && v.birthday.is_none()
            {
                plain.push((v, b.id, s.meta.name.clone(), s.meta.etag()));
            }
        }
    }
    let shared_ids: HashSet<Uuid> =
        sqlx::query_scalar("SELECT DISTINCT collection_id FROM grants").fetch_all(pool).await?.into_iter().collect();
    let mut out = Vec::new();
    for c in collections::visible(pool, actor, CollectionKind::Calendar).await? {
        if !c.access.can_write() || c.filled() {
            continue;
        }
        let calendar = if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() };
        for s in super::objects::all(pool, actor, c.id).await? {
            let Some(m) = birthdays::manual(&s.body) else { continue };
            let (month, day) = (m.start.month(), m.start.day());
            let named: Vec<&(Person, Uuid, String)> =
                found.iter().filter(|(p, ..)| birthdays::names(&m.title, &p.view)).collect();
            let covered = named.iter().find(|(p, ..)| p.birthday.is_some_and(|d| d.month == month && d.day == day));
            let f = if let Some((p, ..)) = covered {
                Found::Covered { contact: birthdays::shown_name(&p.view) }
            } else if let Some((v, book, name, etag)) = plain.iter().find(|(v, ..)| birthdays::names(&m.title, v)) {
                Found::Copy { contact: birthdays::shown_name(v), book: *book, name: name.clone(), etag: etag.clone() }
            } else if let Some((p, ..)) = named.iter().find(|(p, ..)| p.birthday.is_some()) {
                Found::Differs {
                    contact: birthdays::shown_name(&p.view),
                    birthday: p.birthday.expect("found with one"),
                }
            } else {
                continue;
            };
            out.push(ManualEvent {
                collection: c.id,
                calendar: calendar.clone(),
                shared: c.owner_id != actor || shared_ids.contains(&c.id),
                name: s.meta.name.clone(),
                etag: s.meta.etag(),
                title: m.title,
                start: m.start,
                found: f,
            });
        }
    }
    out.sort_by(|a, b| (a.start.month(), a.start.day(), &a.title).cmp(&(b.start.month(), b.start.day(), &b.title)));
    Ok(out)
}

/// One manual event to remove, with the date to copy to its contact first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Removal {
    pub event: Target,
    pub copy: Option<(Target, Day)>,
}

/// Remove manual birthday events and copy dates to contacts, all as one
/// change (one Undo). Each event must still be a manual yearly event and
/// each contact still without a birthday, at the ETags the page saw.
pub async fn remove_manual(
    pool: &PgPool,
    actor: Actor,
    removals: &[Removal],
    now: chrono::NaiveDateTime,
) -> Result<Uuid> {
    of(pool, actor).await?.ok_or(DomainError::NotFound)?;
    let mut targets: Vec<Target> = Vec::new();
    // What each target becomes: None deletes an event, Some(day) sets a card's birthday.
    let mut plan: Vec<Option<Day>> = Vec::new();
    for r in removals {
        targets.push(r.event.clone());
        plan.push(None);
        if let Some((card, day)) = &r.copy
            && !targets.iter().any(|t| t.collection == card.collection && t.name == card.name)
        {
            targets.push(card.clone());
            plan.push(Some(*day));
        }
    }
    let change = events::write_set(pool, actor, &targets, |texts| {
        texts
            .iter()
            .zip(&plan)
            .map(|(text, p)| match p {
                None => {
                    if birthdays::manual(text).is_none() {
                        return Err(DomainError::PreconditionFailed);
                    }
                    Ok(None)
                }
                Some(day) => {
                    let v = card::read(text).map_err(|e| DomainError::Invalid(e.to_string()))?;
                    if v.birthday.is_some() {
                        return Err(DomainError::PreconditionFailed);
                    }
                    let change = CardChange { birthday: Some(Some(day.birthday())), ..CardChange::default() };
                    card::apply(text, &change, now).map(Some).map_err(|e| DomainError::Invalid(e.to_string()))
                }
            })
            .collect()
    })
    .await?;
    if let Some(s) = of(pool, actor).await? {
        rebuild(pool, s.collection).await?;
    }
    Ok(change)
}

/// The card a Birthdays event links to, when the viewer may open it:
/// `(address book, card name)`.
pub async fn contact_of(pool: &PgPool, actor: Actor, text: &str) -> Option<(Uuid, String)> {
    let doc = calzone_core::lines::Document::parse(text).ok()?;
    let ev = doc.components.first()?.components().find(|c| c.name == "VEVENT")?;
    let v = calzone_core::object::unescape(ev.property(birthdays::CONTACT)?.value());
    let (book, name) = v.split_once('/')?;
    let book: Uuid = book.parse().ok()?;
    access::of(pool, actor, book).await.ok().flatten()?;
    Some((book, name.to_string()))
}
