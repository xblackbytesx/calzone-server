//! The system bridge's store side (decisions 49 and 50): what to write
//! into Android's calendar and contacts for a stored object, and Android's
//! edits as pending edits. The mappings are the core's
//! (`calzone_core::android`, `calzone_core::android_contacts`); this adds
//! the store, the clock and the phone's zone.

use calzone_core::android::{self, EventRow, EventRows};
use calzone_core::android_contacts::{self as contacts, ContactRow};
use chrono::Utc;

use crate::store::Store;
use crate::{Error, Result};

fn now() -> chrono::NaiveDateTime {
    Utc::now().naive_utc()
}

fn body(store: &Store, collection: &str, name: &str) -> Result<String> {
    store
        .object(collection, name)?
        .map(|o| o.body)
        .ok_or_else(|| Error::Input(format!("no object {name} in {collection}")))
}

fn bridged(e: android::BridgeError) -> Error {
    Error::Input(e.to_string())
}

/// The rows for one stored object, or `None` when it is gone or not
/// something Android's calendar holds (a task, a journal).
pub fn event_rows(store: &Store, collection: &str, name: &str, phone_zone: &str) -> Result<Option<EventRows>> {
    Ok(store.object(collection, name)?.and_then(|o| android::rows(&o.body, android::zone(phone_zone))))
}

/// An edited row (the series, or an exception) of an object the store has.
/// Only what changed by meaning becomes an edit; returns whether anything
/// did.
pub fn apply_row(store: &Store, collection: &str, name: &str, row: &EventRow, phone_zone: &str) -> Result<bool> {
    let text = body(store, collection, name)?;
    let out = android::apply(&text, row, android::zone(phone_zone), now(), "").map_err(bridged)?;
    if out.as_deref() == Some(text.as_str()) {
        return Ok(false);
    }
    store.edit(collection, name, out.as_deref())?;
    Ok(true)
}

/// A row another app made: a new object named `<uid>.ics`. Returns the name.
pub fn create_from_row(
    store: &Store,
    collection: &str,
    row: &EventRow,
    phone_zone: &str,
    uid: &str,
    fallback_title: &str,
) -> Result<String> {
    let text = android::new_event(row, uid, android::zone(phone_zone), now(), fallback_title).map_err(bridged)?;
    let name = format!("{uid}.ics");
    store.edit(collection, &name, Some(&text))?;
    Ok(name)
}

/// A deleted row: the whole object, or (`original_instance`) one occurrence.
pub fn delete(
    store: &Store,
    collection: &str,
    name: &str,
    original_instance: Option<i64>,
    phone_zone: &str,
) -> Result<()> {
    let Some(original) = original_instance else {
        return store.edit(collection, name, None);
    };
    let text = body(store, collection, name)?;
    let canceled =
        EventRow { status: Some(android::STATUS_CANCELED), original_instance: Some(original), ..EventRow::default() };
    let out = android::apply(&text, &canceled, android::zone(phone_zone), now(), "").map_err(bridged)?;
    store.edit(collection, name, out.as_deref())
}

/// The event moved to another calendar in another app: its (edited) text
/// goes to `to` under the same name and leaves `from`; both edits go up in
/// the next run.
pub fn move_to(store: &Store, from: &str, to: &str, name: &str, row: &EventRow, phone_zone: &str) -> Result<()> {
    let text = body(store, from, name)?;
    let out = android::apply(&text, row, android::zone(phone_zone), now(), "").map_err(bridged)?;
    let Some(out) = out else { return store.edit(from, name, None) };
    store.edit(to, name, Some(&out))?;
    store.edit(from, name, None)
}

/// The raw contact for a stored vCard, or `None` when it is gone or not a
/// contact Android holds (a group card).
pub fn contact_row(store: &Store, collection: &str, name: &str) -> Result<Option<ContactRow>> {
    Ok(store.object(collection, name)?.and_then(|o| contacts::row(&o.body)))
}

/// An edited raw contact of a stored vCard. Only what changed by meaning
/// becomes an edit; returns whether anything did.
pub fn apply_contact(store: &Store, collection: &str, name: &str, row: &ContactRow) -> Result<bool> {
    let text = body(store, collection, name)?;
    let out = contacts::apply(&text, row, now()).map_err(|e| Error::Input(e.to_string()))?;
    if out == text {
        return Ok(false);
    }
    store.edit(collection, name, Some(&out))?;
    Ok(true)
}

/// A raw contact another app made: a new vCard named `<uid>.vcf`. Returns
/// the name.
pub fn create_contact(
    store: &Store,
    collection: &str,
    row: &ContactRow,
    uid: &str,
    fallback_name: &str,
) -> Result<String> {
    let text = contacts::new_contact(row, uid, now(), fallback_name).map_err(|e| Error::Input(e.to_string()))?;
    let name = format!("{uid}.vcf");
    store.edit(collection, &name, Some(&text))?;
    Ok(name)
}

#[cfg(test)]
mod tests {
    use calzone_core::sync::{self as wire, Batch, Change, State};

    use super::*;

    fn collection(id: &str) -> wire::Collection {
        wire::Collection {
            id: id.into(),
            kind: "calendar".into(),
            owner: "anna".into(),
            owner_name: "Anna".into(),
            name: id.into(),
            description: String::new(),
            color: None,
            components: Vec::new(),
            access: wire::Access::Owner,
            read_only: false,
            subscribed: false,
            state: State(1),
            mine: wire::Mine::default(),
        }
    }

    const EVENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:e\r\nDTSTAMP:20260101T000000Z\r\n\
        DTSTART:20260310T090000Z\r\nDTEND:20260310T100000Z\r\nSUMMARY:Dentist\r\nX-KEEP:yes\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn store() -> Store {
        let s = Store::in_memory().unwrap();
        s.set_collections(&[collection("a"), collection("b")], 0).unwrap();
        let batch = Batch {
            collection: "a".into(),
            since: State(0),
            state: State(1),
            changes: vec![Change { name: "e.ics".into(), revision: State(1), body: Some(EVENT.into()) }],
            more: false,
        };
        s.apply_batch(&batch, None).unwrap();
        s
    }

    #[test]
    fn edits_moves_and_new_events() {
        let s = store();
        let rows = event_rows(&s, "a", "e.ics", "Europe/Amsterdam").unwrap().unwrap();
        assert!(!apply_row(&s, "a", "e.ics", &rows.main, "Europe/Amsterdam").unwrap(), "unchanged is no edit");
        assert!(s.pending("a").unwrap().is_empty());

        let mut row = rows.main.clone();
        row.location = Some("Main street".into());
        assert!(apply_row(&s, "a", "e.ics", &row, "Europe/Amsterdam").unwrap());
        let body = s.object("a", "e.ics").unwrap().unwrap().body;
        assert!(body.contains("LOCATION:Main street") && body.contains("X-KEEP:yes"));

        move_to(&s, "a", "b", "e.ics", &row, "Europe/Amsterdam").unwrap();
        assert!(s.object("a", "e.ics").unwrap().is_none());
        assert!(s.object("b", "e.ics").unwrap().unwrap().body.contains("X-KEEP:yes"));
        assert_eq!(s.pending("a").unwrap().len(), 1, "the delete goes up");
        assert_eq!(s.pending("b").unwrap().len(), 1, "and the create");

        let new = EventRow {
            title: Some("Party".into()),
            dtstart: row.dtstart,
            timezone: "UTC".into(),
            ..EventRow::default()
        };
        let name = create_from_row(&s, "a", &new, "Europe/Amsterdam", "u-1", "(untitled)").unwrap();
        assert_eq!(name, "u-1.ics");
        assert!(s.object("a", &name).unwrap().unwrap().body.contains("SUMMARY:Party"));
        delete(&s, "a", &name, None, "Europe/Amsterdam").unwrap();
        assert!(s.object("a", &name).unwrap().is_none());
        assert!(matches!(apply_row(&s, "a", "nope.ics", &row, "UTC"), Err(Error::Input(_))));
    }

    const CARD: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:Bea Smith\r\nN:Smith;Bea;;;\r\n\
        TEL;TYPE=cell:+31 6 1\r\nX-KEEP:yes\r\nEND:VCARD\r\n";

    #[test]
    fn contact_edits_and_new_contacts() {
        let s = Store::in_memory().unwrap();
        let mut book = collection("book");
        book.kind = "addressbook".into();
        s.set_collections(&[book], 0).unwrap();
        let batch = Batch {
            collection: "book".into(),
            since: State(0),
            state: State(1),
            changes: vec![Change { name: "c.vcf".into(), revision: State(1), body: Some(CARD.into()) }],
            more: false,
        };
        s.apply_batch(&batch, None).unwrap();

        let row = contact_row(&s, "book", "c.vcf").unwrap().unwrap();
        assert!(!apply_contact(&s, "book", "c.vcf", &row).unwrap(), "unchanged is no edit");
        assert!(s.pending("book").unwrap().is_empty());
        let mut edited = row.clone();
        edited.note = "Met at the pool".into();
        assert!(apply_contact(&s, "book", "c.vcf", &edited).unwrap());
        let body = s.object("book", "c.vcf").unwrap().unwrap().body;
        assert!(body.contains("NOTE:Met at the pool") && body.contains("X-KEEP:yes"));
        assert_eq!(s.pending("book").unwrap().len(), 1);

        let name = create_contact(&s, "book", &edited, "u-2", "(no name)").unwrap();
        assert_eq!(name, "u-2.vcf");
        assert!(s.object("book", &name).unwrap().unwrap().body.contains("FN:Bea Smith"));
        assert!(contact_row(&s, "book", "gone.vcf").unwrap().is_none());
    }
}
