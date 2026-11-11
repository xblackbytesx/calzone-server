//! CalZone's own calendar screens (phase 4b, decision 52): what they show
//! and what they write, on top of the store. Occurrences come from the
//! core (decision 9) in the phone's zone with the person's own view of each
//! calendar (decision 8); edits go through the core's lossless editor with
//! the recurring scope, as on the web (`calzone_core::view`), and wait as
//! pending edits like any other. Each write hands back what it replaced,
//! so the screen can offer Undo for a few seconds.
//!
//! Expanding a repeating event costs time from its first occurrence on,
//! whatever the range (calcard has no "from"): an open-ended weekly series
//! from years ago is hundreds of occurrences on every call. So each event
//! is expanded once over a window of about three years and kept in an
//! [`Expansions`] cache, keyed by its text and the phone's zone; scrolling
//! and paging filter that. Measured on the household corpus: a month took
//! about 140 ms on a desk machine without it (decision 52).

use calzone_core::edit::{self, Scope};
use calzone_core::lines::Document;
use calzone_core::moment::Key;
use calzone_core::quickadd::Lang;
use calzone_core::sync::{Access, Collection, Mine};
use calzone_core::view::{self, Detail, EventInput, Occurrence, QuickDraft};
use chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Mutex;

use crate::store::Store;
use crate::{Error, Result};

fn zone(name: &str) -> chrono_tz::Tz {
    name.parse().unwrap_or(chrono_tz::UTC)
}

fn utc(seconds: i64) -> NaiveDateTime {
    DateTime::<Utc>::from_timestamp(seconds, 0).unwrap_or_default().naive_utc()
}

/// How a calendar shows for this person: their own name and color for it
/// when they set one, the owner's initials when it is not theirs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shown {
    pub calendar: String,
    pub color: String,
    /// The owner's display name, and its initials.
    pub owner: String,
    pub initials: String,
    pub own: bool,
    /// The person may change its events (else only their own reminders).
    pub editable: bool,
}

fn shown(c: &Collection) -> Shown {
    let owner = if c.owner_name.trim().is_empty() { c.owner.clone() } else { c.owner_name.clone() };
    Shown {
        calendar: c.mine.name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| c.name.clone()),
        color: c.mine.color.clone().or_else(|| c.color.clone()).unwrap_or_else(|| view::DEFAULT_COLOR.into()),
        initials: view::initials(&owner),
        owner,
        own: c.access == Access::Owner,
        editable: !c.read_only,
    }
}

/// One occurrence on a screen.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Item {
    pub collection: String,
    pub name: String,
    #[serde(flatten)]
    pub occurrence: Occurrence,
    #[serde(flatten)]
    pub shown: Shown,
    /// An edit made here is still waiting for the server.
    pub pending: bool,
}

/// How far around a requested range one expansion reaches (seconds).
const BEFORE: i64 = 400 * 86_400;
const AFTER: i64 = 800 * 86_400;

/// Each event's occurrences over a window, as last expanded (see the
/// module notes). One per process; safe from any thread.
#[derive(Default)]
pub struct Expansions {
    by_object: Mutex<HashMap<(String, String), Expanded>>,
}

struct Expanded {
    /// The text and zone it was expanded for.
    key: u64,
    from: i64,
    to: i64,
    occurrences: Vec<Occurrence>,
}

fn text_key(body: &str, phone_zone: &str) -> u64 {
    let mut h = DefaultHasher::new();
    body.hash(&mut h);
    phone_zone.hash(&mut h);
    h.finish()
}

/// The same overlap rule as the core's: a zero-length occurrence counts
/// when it starts in the range.
fn overlaps(o: &Occurrence, from: i64, to: i64) -> bool {
    o.at < to && (o.end_at > from || (o.end_at == o.at && o.at >= from))
}

impl Expansions {
    pub fn new() -> Expansions {
        Expansions::default()
    }

    /// The occurrences of one object in `[from, to)`, from the cache when
    /// it covers the range for this text and zone.
    fn of(&self, collection: &str, name: &str, body: &str, from: i64, to: i64, phone_zone: &str) -> Vec<Occurrence> {
        let key = text_key(body, phone_zone);
        let id = (collection.to_string(), name.to_string());
        let mut map = self.by_object.lock().unwrap_or_else(|p| p.into_inner());
        let hit = map.get(&id).is_some_and(|e| e.key == key && e.from <= from && to <= e.to);
        if !hit {
            let (wf, wt) = (from.saturating_sub(BEFORE), to.max(from.saturating_add(AFTER)));
            let occurrences = view::occurrences(body, utc(wf), utc(wt), zone(phone_zone));
            map.insert(id.clone(), Expanded { key, from: wf, to: wt, occurrences });
        }
        map[&id].occurrences.iter().filter(|o| overlaps(o, from, to)).cloned().collect()
    }

    /// Forget objects no longer in the store (deleted, or their collection
    /// gone), so the cache does not grow with what passed through.
    fn keep_only(&self, alive: &std::collections::HashSet<(String, String)>) {
        self.by_object.lock().unwrap_or_else(|p| p.into_inner()).retain(|k, _| alive.contains(k));
    }
}

/// Every event occurring in `[from, to)` (UTC seconds) in the calendars
/// this phone shows (`hidden`: the ones switched off here), in `phone_zone`,
/// sorted by start, all-day first.
pub fn occurrences(
    store: &Store,
    cache: &Expansions,
    from: i64,
    to: i64,
    phone_zone: &str,
    hidden: &[String],
) -> Result<Vec<Item>> {
    let collections = store.collections()?;
    let mut out = Vec::new();
    // The store's bounds are UTC; floating and all-day events are widened
    // by 14 hours there (decision 27), so this catches every zone.
    for meta in store.in_range(from, to)? {
        if meta.kind != "VEVENT" || hidden.contains(&meta.collection) {
            continue;
        }
        let Some(c) = collections.iter().find(|c| c.info.id == meta.collection) else { continue };
        if c.info.kind != "calendar" {
            continue;
        }
        let Some(obj) = store.object(&meta.collection, &meta.name)? else { continue };
        let s = shown(&c.info);
        for o in cache.of(&meta.collection, &meta.name, &obj.body, from, to, phone_zone) {
            out.push(Item {
                collection: meta.collection.clone(),
                name: meta.name.clone(),
                occurrence: o,
                shown: s.clone(),
                pending: meta.pending,
            });
        }
    }
    if cache.by_object.lock().unwrap_or_else(|p| p.into_inner()).len() > 4096 {
        let mut alive = std::collections::HashSet::new();
        for c in &collections {
            for m in store.objects(&c.info.id)? {
                alive.insert((c.info.id.clone(), m.name));
            }
        }
        cache.keep_only(&alive);
    }
    out.sort_by(|a, b| {
        (!a.occurrence.all_day, a.occurrence.at, &a.occurrence.title).cmp(&(
            !b.occurrence.all_day,
            b.occurrence.at,
            &b.occurrence.title,
        ))
    });
    Ok(out)
}

/// One event (or occurrence) as the event sheet shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Event {
    pub collection: String,
    pub name: String,
    #[serde(flatten)]
    pub detail: Detail,
    #[serde(flatten)]
    pub shown: Shown,
    pub pending: bool,
}

fn collection(store: &Store, id: &str) -> Result<Collection> {
    store
        .collections()?
        .into_iter()
        .map(|c| c.info)
        .find(|c| c.id == id)
        .ok_or_else(|| Error::Input(format!("no collection {id}")))
}

fn instance(k: Option<&str>) -> Result<Option<Key>> {
    k.map(|k| k.parse::<Key>().map_err(|_| Error::Input(format!("{k:?} is not an occurrence")))).transpose()
}

/// The event `name` (`instance`: one occurrence of it), or `None` when it
/// is gone or not an event.
pub fn event(
    store: &Store,
    collection_id: &str,
    name: &str,
    at: Option<&str>,
    phone_zone: &str,
) -> Result<Option<Event>> {
    let c = collection(store, collection_id)?;
    let Some(obj) = store.object(collection_id, name)? else { return Ok(None) };
    let Some(detail) = view::detail(&obj.body, instance(at)?, zone(phone_zone)) else { return Ok(None) };
    let pending = store.objects(collection_id)?.iter().any(|m| m.name == name && m.pending);
    Ok(Some(Event { collection: collection_id.into(), name: name.into(), detail, shown: shown(&c), pending }))
}

/// How far back and ahead search looks for an event's occurrence to show.
const SEARCH_SPAN: i64 = 2 * 365 * 86_400;

/// Events whose title, place or notes hold every word of `query` (case
/// does not matter), in the calendars this phone shows: one occurrence each,
/// the next one still to end after `now`, else the latest before it.
/// Coming ones first (soonest first), then past ones (latest first); at most
/// `limit`. An event with no occurrence within two years of now is left out.
pub fn search(
    store: &Store,
    cache: &Expansions,
    query: &str,
    now: i64,
    phone_zone: &str,
    hidden: &[String],
    limit: usize,
) -> Result<Vec<Item>> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    if words.is_empty() {
        return Ok(Vec::new());
    }
    let mut coming = Vec::new();
    let mut past = Vec::new();
    for c in store.collections()? {
        if c.info.kind != "calendar" || hidden.contains(&c.info.id) {
            continue;
        }
        let s = shown(&c.info);
        for meta in store.objects(&c.info.id)? {
            if meta.kind != "VEVENT" {
                continue;
            }
            let Some(obj) = store.object(&c.info.id, &meta.name)? else { continue };
            if !view::matches(&obj.body, &words) {
                continue;
            }
            let all = cache.of(&c.info.id, &meta.name, &obj.body, now - SEARCH_SPAN, now + SEARCH_SPAN, phone_zone);
            let pick = all.iter().find(|o| o.end_at > now || (o.end_at == o.at && o.at >= now)).cloned();
            let (o, upcoming) = match pick {
                Some(o) => (o, true),
                None => match all.into_iter().filter(|o| o.at <= now).max_by_key(|o| o.at) {
                    Some(o) => (o, false),
                    None => continue,
                },
            };
            let item = Item {
                collection: c.info.id.clone(),
                name: meta.name.clone(),
                occurrence: o,
                shown: s.clone(),
                pending: meta.pending,
            };
            if upcoming { coming.push(item) } else { past.push(item) }
        }
    }
    coming.sort_by(|a, b| (a.occurrence.at, &a.occurrence.title).cmp(&(b.occurrence.at, &b.occurrence.title)));
    past.sort_by(|a, b| (b.occurrence.at, &a.occurrence.title).cmp(&(a.occurrence.at, &b.occurrence.title)));
    coming.extend(past);
    coming.truncate(limit);
    Ok(coming)
}

/// A reminder due (decision 51): the instant it rings (UTC seconds) and
/// the occurrence it is for, as the screens show it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Due {
    pub fire: i64,
    pub occurrence: Item,
}

/// How long before its occurrence a reminder can ring and still be found.
const LEAD: i64 = 35 * 86_400;
/// How long after the start (or end) a reminder can ring ("after the end").
const LAG: i64 = 2 * 86_400;

/// My reminders that ring in `[from, to)`, soonest first, for the calendars
/// this phone shows (`hidden` as for [`occurrences`]). Each occurrence uses
/// its own part's reminders: an overridden one has its own. A reminder set
/// more than 35 days before its event is not found.
pub fn reminders(
    store: &Store,
    cache: &Expansions,
    from: i64,
    to: i64,
    phone_zone: &str,
    hidden: &[String],
) -> Result<Vec<Due>> {
    let items = occurrences(store, cache, from.saturating_sub(LAG), to.saturating_add(LEAD), phone_zone, hidden)?;
    let mut by_object: HashMap<(String, String), view::Reminders> = HashMap::new();
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for it in items {
        let key = (it.collection.clone(), it.name.clone());
        if !by_object.contains_key(&key) {
            let r = store.object(&it.collection, &it.name)?.map(|o| view::reminders_of(&o.body)).unwrap_or_default();
            by_object.insert(key.clone(), r);
        }
        let triggers = by_object[&key].of(it.occurrence.instance.as_deref());
        for t in triggers {
            let fire = t.fire(it.occurrence.at, it.occurrence.end_at);
            // A fixed instant on a series rings once, not for every occurrence.
            let once = if matches!(t, view::Trigger::At(_)) { None } else { it.occurrence.instance.clone() };
            if fire >= from && fire < to && seen.insert((key.clone(), once, fire)) {
                out.push(Due { fire, occurrence: it.clone() });
            }
        }
    }
    out.sort_by(|a, b| {
        (a.fire, a.occurrence.occurrence.at, &a.occurrence.occurrence.title).cmp(&(
            b.fire,
            b.occurrence.occurrence.at,
            &b.occurrence.occurrence.title,
        ))
    });
    Ok(out)
}

/// What a write replaced, for Undo: each object's text before and after
/// (`None`: it did not exist, or was deleted).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Undo {
    pub objects: Vec<Replaced>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replaced {
    pub collection: String,
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

/// A write's result: the object's name (a new event's), a second object a
/// "this and following" split made, and what Undo needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Written {
    pub name: String,
    pub created: Option<String>,
    pub undo: Undo,
}

fn write(
    store: &Store,
    collection: &str,
    name: &str,
    before: Option<String>,
    after: Option<String>,
) -> Result<Replaced> {
    store.edit_here(collection, name, after.as_deref())?;
    Ok(Replaced { collection: collection.into(), name: name.into(), before, after })
}

fn edit_error(e: edit::EditError) -> Error {
    Error::Input(e.to_string())
}

/// The UID of the event an editor made.
fn uid_of(text: &str) -> Option<String> {
    let doc = Document::parse(text).ok()?;
    let ev = doc.components.first()?.components().find(|c| c.name == "VEVENT")?;
    ev.property("UID").map(|l| l.value().to_string())
}

/// A new event in `collection`, named `<uid>.ics`. Without reminders in
/// the input, the person's default reminder for that calendar applies.
pub fn create(
    store: &Store,
    collection_id: &str,
    input: &EventInput,
    phone_zone: &str,
    uid: &str,
    now: i64,
) -> Result<Written> {
    let c = collection(store, collection_id)?;
    if c.read_only || c.kind != "calendar" {
        return Err(Error::Input("this calendar is view only".into()));
    }
    let mut change = view::change_for(None, None, input, zone(phone_zone));
    if change.alarms.is_none() {
        change.alarms = c.mine.default_alarm.map(|m| vec![i64::from(m)]);
    }
    let text = edit::new_event(&change, uid, utc(now)).map_err(edit_error)?;
    let name = format!("{uid}.ics");
    let r = write(store, collection_id, &name, None, Some(text))?;
    Ok(Written { name, created: None, undo: Undo { objects: vec![r] } })
}

/// Change an event, or (`at`, `scope`) occurrences of a recurring one. A
/// change of the person's own reminders only covers every occurrence and
/// is allowed on a calendar they can only view (decision 8); anything else
/// there is refused here, as the server would. `new_uid` names the new
/// series a "this and following" change makes.
#[allow(clippy::too_many_arguments)]
pub fn update(
    store: &Store,
    collection_id: &str,
    name: &str,
    at: Option<&str>,
    scope: Scope,
    input: &EventInput,
    phone_zone: &str,
    new_uid: &str,
    now: i64,
) -> Result<Written> {
    let c = collection(store, collection_id)?;
    let text =
        store.object(collection_id, name)?.map(|o| o.body).ok_or_else(|| Error::Input(format!("no event {name}")))?;
    let outcome = match input.reminders_only() {
        Some(alarms) => {
            edit::Outcome { updated: Some(edit::set_alarms(&text, alarms).map_err(edit_error)?), created: None }
        }
        None if c.read_only => return Err(Error::Input("this calendar is view only".into())),
        None => {
            let change = view::change_for(Some(&text), instance(at)?, input, zone(phone_zone));
            edit::edit(&text, instance(at)?, scope, &change, utc(now), new_uid).map_err(edit_error)?
        }
    };
    apply(store, collection_id, name, text, outcome)
}

/// Delete an event, one occurrence, or an occurrence and the ones after it.
pub fn remove(
    store: &Store,
    collection_id: &str,
    name: &str,
    at: Option<&str>,
    scope: Scope,
    now: i64,
) -> Result<Written> {
    if collection(store, collection_id)?.read_only {
        return Err(Error::Input("this calendar is view only".into()));
    }
    let text =
        store.object(collection_id, name)?.map(|o| o.body).ok_or_else(|| Error::Input(format!("no event {name}")))?;
    let outcome = edit::delete(&text, instance(at)?, scope, utc(now)).map_err(edit_error)?;
    apply(store, collection_id, name, text, outcome)
}

fn apply(store: &Store, collection_id: &str, name: &str, text: String, outcome: edit::Outcome) -> Result<Written> {
    let mut objects = Vec::new();
    if outcome.updated.as_deref() != Some(text.as_str()) {
        objects.push(write(store, collection_id, name, Some(text), outcome.updated)?);
    }
    let mut created = None;
    if let Some(new) = outcome.created {
        let uid = uid_of(&new).ok_or_else(|| Error::Input("the new series has no UID".into()))?;
        let new_name = format!("{uid}.ics");
        objects.push(write(store, collection_id, &new_name, None, Some(new))?);
        created = Some(new_name);
    }
    Ok(Written { name: name.into(), created, undo: Undo { objects } })
}

/// Undo a write: every object it touched goes back, newest first. Refused
/// (`Ok(false)`, nothing changed) when any of them changed since, by a sync
/// or another edit.
pub fn undo(store: &Store, u: &Undo) -> Result<bool> {
    for r in &u.objects {
        let now = store.object(&r.collection, &r.name)?.map(|o| o.body);
        if now != r.after {
            return Ok(false);
        }
    }
    for r in u.objects.iter().rev() {
        store.edit_here(&r.collection, &r.name, r.before.as_deref())?;
    }
    Ok(true)
}

/// "dentist thu 3pm" in `lang` (`en` or `nl`), read at `now` (UTC seconds)
/// in the phone's zone; `None` when there is nothing to make a title of.
pub fn quick_add(text: &str, lang: &str, phone_zone: &str, now: i64) -> Option<QuickDraft> {
    let lang = if lang.eq_ignore_ascii_case("nl") { Lang::Nl } else { Lang::En };
    let local_now = zone(phone_zone).from_utc_datetime(&utc(now)).naive_local();
    view::quick_draft(text, lang, local_now)
}

/// Change the person's own view of a calendar or address book: shown at
/// once, on the server with the next sync.
pub fn set_mine(store: &Store, collection_id: &str, mine: &Mine) -> Result<()> {
    if mine
        .color
        .as_deref()
        .is_some_and(|c| c.len() != 7 || !c.starts_with('#') || !c[1..].bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err(Error::Input("a color like #3a7bd5".into()));
    }
    store.set_mine(collection_id, mine)
}

#[cfg(test)]
mod tests {
    use calzone_core::sync::{self as wire, Batch, Change, State};
    use calzone_core::view::{Freq, Repeat, TimeInput};

    use super::*;

    fn cal(id: &str, read_only: bool) -> wire::Collection {
        wire::Collection {
            id: id.into(),
            kind: "calendar".into(),
            owner: "bram".into(),
            owner_name: "Bram de Vries".into(),
            name: id.into(),
            description: String::new(),
            color: Some("#112233".into()),
            components: Vec::new(),
            access: if read_only { wire::Access::Read } else { wire::Access::Write },
            read_only,
            subscribed: false,
            state: State(1),
            mine: wire::Mine { default_alarm: Some(15), ..wire::Mine::default() },
        }
    }

    const SWIM: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:swim\r\nDTSTAMP:20260101T000000Z\r\n\
        DTSTART;TZID=Europe/Amsterdam:20260107T160000\r\nDTEND;TZID=Europe/Amsterdam:20260107T170000\r\n\
        SUMMARY:Swim\r\nRRULE:FREQ=WEEKLY\r\nX-KEEP:yes\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn store() -> Store {
        let s = Store::in_memory().unwrap();
        s.set_collections(&[cal("family", false), cal("feed", true)], 0).unwrap();
        for c in ["family", "feed"] {
            let batch = Batch {
                collection: c.into(),
                since: State(0),
                state: State(1),
                changes: vec![Change { name: "swim.ics".into(), revision: State(1), body: Some(SWIM.into()) }],
                more: false,
            };
            s.apply_batch(&batch, None).unwrap();
        }
        s.forget_local_changes(i64::MAX).unwrap();
        s
    }

    fn secs(s: &str) -> i64 {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap().and_utc().timestamp()
    }

    const AMS: &str = "Europe/Amsterdam";

    #[test]
    fn search_finds_title_place_and_notes() {
        const DENTIST: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:d\r\nDTSTAMP:20260101T000000Z\r\n\
            DTSTART:20250310T080000Z\r\nDTEND:20250310T083000Z\r\nSUMMARY:Dentist\r\nLOCATION:Main street 12\r\n\
            DESCRIPTION:Bring the\\, card\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let s = store();
        s.edit("family", "dentist.ics", Some(DENTIST)).unwrap();
        let cache = Expansions::new();
        let now = secs("2026-01-10 12:00");
        let hit = |q: &str| -> Vec<(String, String)> {
            search(&s, &cache, q, now, AMS, &["feed".into()], 50)
                .unwrap()
                .into_iter()
                .map(|i| (i.occurrence.title, i.occurrence.start))
                .collect()
        };
        assert_eq!(hit("swim"), vec![("Swim".into(), "2026-01-14T16:00:00".into())], "the next occurrence of a series");
        assert_eq!(hit("MAIN street"), vec![("Dentist".into(), "2025-03-10T09:00:00".into())], "a past one, by place");
        assert_eq!(hit("card dentist").len(), 1, "every word, from notes and title");
        assert!(hit("dentist swim").is_empty());
        assert!(hit("  ").is_empty());
        let both = search(&s, &cache, "swim", now, AMS, &[], 50).unwrap();
        assert_eq!(both.len(), 2, "both calendars when none is hidden");
    }

    #[test]
    fn reminders_ring_from_each_occurrences_own_part() {
        const ALARMED: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:swim\r\nDTSTAMP:20260101T000000Z\r\n\
            DTSTART;TZID=Europe/Amsterdam:20260107T160000\r\nDTEND;TZID=Europe/Amsterdam:20260107T170000\r\n\
            SUMMARY:Swim\r\nRRULE:FREQ=WEEKLY\r\n\
            BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n\
            BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:PT0S\r\nEND:VALARM\r\nEND:VEVENT\r\n\
            BEGIN:VEVENT\r\nUID:swim\r\nDTSTAMP:20260101T000000Z\r\nRECURRENCE-ID;TZID=Europe/Amsterdam:20260114T160000\r\n\
            DTSTART;TZID=Europe/Amsterdam:20260114T180000\r\nDTEND;TZID=Europe/Amsterdam:20260114T190000\r\n\
            SUMMARY:Swim (late)\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT1H\r\nEND:VALARM\r\nEND:VEVENT\r\n\
            END:VCALENDAR\r\n";
        let s = store();
        for c in ["family", "feed"] {
            s.edit(c, "swim.ics", Some(ALARMED)).unwrap();
        }
        let cache = Expansions::new();
        let due =
            reminders(&s, &cache, secs("2026-01-07 00:00"), secs("2026-01-15 00:00"), AMS, &["feed".into()]).unwrap();
        let got: Vec<(i64, &str)> = due.iter().map(|d| (d.fire, d.occurrence.occurrence.title.as_str())).collect();
        // Amsterdam is UTC+1 in January.
        assert_eq!(
            got,
            vec![
                (secs("2026-01-07 14:45"), "Swim"),
                (secs("2026-01-07 16:00"), "Swim"),
                (secs("2026-01-14 16:00"), "Swim (late)"),
            ],
            "15 minutes before and at the end; the moved occurrence has its own hour before"
        );
        assert!(due.iter().all(|d| d.occurrence.collection == "family"), "a calendar switched off here does not ring");
        let later =
            reminders(&s, &cache, secs("2026-01-07 14:46"), secs("2026-01-07 16:00"), AMS, &["feed".into()]).unwrap();
        assert!(later.is_empty(), "the range is [from, to)");
        assert!(view::reminders_of(SWIM).is_empty());
    }

    #[test]
    fn a_month_with_each_calendars_own_look() {
        let s = store();
        let cache = Expansions::new();
        let jan = occurrences(&s, &cache, secs("2026-01-01 00:00"), secs("2026-02-01 00:00"), AMS, &[]).unwrap();
        assert_eq!(jan.len(), 8, "four Wednesdays in two calendars");
        assert_eq!(jan[0].occurrence.start, "2026-01-07T16:00:00");
        assert_eq!((jan[0].shown.initials.as_str(), jan[0].shown.color.as_str()), ("BV", "#112233"));
        let shown =
            occurrences(&s, &cache, secs("2026-01-01 00:00"), secs("2026-02-01 00:00"), AMS, &["feed".into()]).unwrap();
        assert!(shown.iter().all(|i| i.collection == "family"), "a calendar switched off here is left out");

        set_mine(&s, "family", &Mine { color: Some("#00aa00".into()), name: Some("Gezin".into()), ..Mine::default() })
            .unwrap();
        let one = &occurrences(&s, &cache, secs("2026-01-07 00:00"), secs("2026-01-08 00:00"), AMS, &["feed".into()])
            .unwrap()[0];
        assert_eq!((one.shown.calendar.as_str(), one.shown.color.as_str()), ("Gezin", "#00aa00"));
        assert_eq!(s.pending_mine().unwrap().len(), 1, "sent with the next sync");
        assert_eq!(s.local_changes(0, 10).unwrap().len(), 1, "and the bridge hears of it");
        assert!(set_mine(&s, "family", &Mine { color: Some("red".into()), ..Mine::default() }).is_err());
    }

    #[test]
    fn edits_with_scope_and_undo() {
        let s = store();
        let cache = Expansions::new();
        let input = EventInput { summary: Some("Swimming lesson".into()), ..EventInput::default() };
        let w = update(
            &s,
            "family",
            "swim.ics",
            Some("20260114T160000"),
            Scope::This,
            &input,
            AMS,
            "n1",
            secs("2026-01-02 00:00"),
        )
        .unwrap();
        let body = s.object("family", "swim.ics").unwrap().unwrap().body;
        assert!(body.contains("SUMMARY:Swimming lesson") && body.contains("X-KEEP:yes"), "{body}");
        assert_eq!(s.local_changes(0, 10).unwrap().len(), 1, "Android's calendar shows it at once");
        let ev = event(&s, "family", "swim.ics", Some("20260114T160000"), AMS).unwrap().unwrap();
        assert_eq!((ev.detail.title.as_str(), ev.pending), ("Swimming lesson", true));

        assert!(undo(&s, &w.undo).unwrap());
        assert_eq!(s.object("family", "swim.ics").unwrap().unwrap().body, SWIM);
        assert!(!undo(&s, &w.undo).unwrap(), "only once: it changed since");

        // This and following: a second series.
        let later = EventInput {
            start: Some(TimeInput::Local(NaiveDateTime::parse_from_str("2026-01-21 17:00", "%Y-%m-%d %H:%M").unwrap())),
            ..EventInput::default()
        };
        let w = update(
            &s,
            "family",
            "swim.ics",
            Some("20260121T160000"),
            Scope::Following,
            &later,
            AMS,
            "n2",
            secs("2026-01-02 00:00"),
        )
        .unwrap();
        assert_eq!(w.created.as_deref(), Some("n2.ics"));
        let feb =
            occurrences(&s, &cache, secs("2026-02-01 00:00"), secs("2026-02-08 00:00"), AMS, &["feed".into()]).unwrap();
        assert_eq!(feb[0].occurrence.start, "2026-02-04T17:00:00");
        assert!(undo(&s, &w.undo).unwrap());
        assert!(s.object("family", "n2.ics").unwrap().is_none(), "undo takes the new series away again");
        assert_eq!(s.object("family", "swim.ics").unwrap().unwrap().body, SWIM);

        // One occurrence deleted.
        let w =
            remove(&s, "family", "swim.ics", Some("20260114T160000"), Scope::This, secs("2026-01-02 00:00")).unwrap();
        let jan =
            occurrences(&s, &cache, secs("2026-01-01 00:00"), secs("2026-02-01 00:00"), AMS, &["feed".into()]).unwrap();
        assert_eq!(jan.len(), 3, "the cache follows the edit");
        assert!(undo(&s, &w.undo).unwrap());
    }

    #[test]
    fn new_events_view_only_calendars_and_quick_add() {
        let s = store();
        let input = EventInput {
            summary: Some("Zwemles".into()),
            start: Some(TimeInput::Local(NaiveDateTime::parse_from_str("2026-01-07 16:00", "%Y-%m-%d %H:%M").unwrap())),
            end: Some(TimeInput::Local(NaiveDateTime::parse_from_str("2026-01-07 17:00", "%Y-%m-%d %H:%M").unwrap())),
            repeat: Some(Some(Repeat {
                freq: Freq::Weekly,
                until: chrono::NaiveDate::from_ymd_opt(2026, 7, 1),
                count: None,
            })),
            ..EventInput::default()
        };
        let w = create(&s, "family", &input, AMS, "z1", secs("2026-01-02 00:00")).unwrap();
        assert_eq!(w.name, "z1.ics");
        let ev = event(&s, "family", "z1.ics", None, AMS).unwrap().unwrap();
        assert_eq!((ev.detail.repeat, ev.detail.repeat_until.as_deref()), ("weekly", Some("2026-07-01")));
        assert_eq!(ev.detail.alarms, vec![15], "the calendar's default reminder");
        assert!(create(&s, "feed", &input, AMS, "z2", 0).is_err(), "view only");

        // On a view-only calendar only my own reminders change.
        let mine = EventInput { alarms: Some(vec![60]), ..EventInput::default() };
        update(&s, "feed", "swim.ics", None, Scope::All, &mine, AMS, "x", secs("2026-01-02 00:00")).unwrap();
        assert!(s.object("feed", "swim.ics").unwrap().unwrap().body.contains("TRIGGER:-PT1H"));
        let title = EventInput { summary: Some("x".into()), ..EventInput::default() };
        assert!(update(&s, "feed", "swim.ics", None, Scope::All, &title, AMS, "x", 0).is_err());
        assert!(remove(&s, "feed", "swim.ics", None, Scope::All, 0).is_err());

        let d = quick_add("tandarts do 15u", "nl", AMS, secs("2026-01-05 08:00")).unwrap();
        assert_eq!((d.title.as_str(), d.start.as_deref()), ("tandarts", Some("2026-01-08T15:00:00")));
        assert!(quick_add("   ", "en", AMS, 0).is_none());
    }

    /// The household corpus through the phone's store (confidential: counts
    /// and times only). SKIP without `fixtures/private`.
    #[test]
    fn private_corpus_months_on_the_phone() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private");
        let Ok(rd) = std::fs::read_dir(&dir) else {
            eprintln!("SKIP: no fixtures/private");
            return;
        };
        let mut objects = Vec::new();
        for f in rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == "ics")) {
            let text = std::fs::read_to_string(&f).unwrap_or_else(|_| panic!("private file is not UTF-8"));
            objects.extend(
                calzone_core::import::split_calendar(&text).unwrap_or_else(|_| panic!("does not split")).objects,
            );
        }
        if objects.is_empty() {
            eprintln!("SKIP: no fixtures/private/*.ics");
            return;
        }
        let s = Store::in_memory().unwrap();
        s.set_collections(&[cal("home", false)], 0).unwrap();
        let changes: Vec<Change> = objects
            .iter()
            .enumerate()
            .map(|(i, o)| Change { name: format!("{i}.ics"), revision: State(1), body: Some(o.clone()) })
            .collect();
        let n = changes.len();
        s.apply_batch(
            &Batch { collection: "home".into(), since: State(0), state: State(1), changes, more: false },
            None,
        )
        .unwrap();
        let cache = Expansions::new();
        let month = |m: u32| {
            let from = secs(&format!("2026-{m:02}-01 00:00"));
            let to = if m == 12 { secs("2027-01-01 00:00") } else { secs(&format!("2026-{:02}-01 00:00", m + 1)) };
            (from, to)
        };
        let t = std::time::Instant::now();
        let (f, e) = month(11);
        let first = occurrences(&s, &cache, f, e, AMS, &[]).unwrap().len();
        let cold = t.elapsed();
        let t = std::time::Instant::now();
        let mut seen = 0;
        for m in 1..=12 {
            let (f, e) = month(m);
            seen += occurrences(&s, &cache, f, e, AMS, &[]).unwrap().len();
        }
        let warm = t.elapsed() / 12;
        eprintln!(
            "private corpus on the phone store: objects={n} first_month={first} year={seen} \
             cold_month_ms={} warm_month_us={} (build profile decides)",
            cold.as_millis(),
            warm.as_micros()
        );
    }
}
