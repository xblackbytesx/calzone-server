//! The per-user layer over shared calendar data (decision 8).
//!
//! On a calendar more than one person sees, reminders and the client state
//! around them are personal: my alarm must not ring on your phone, and my
//! dismissing it in Thunderbird must not dismiss yours. [`split`] takes
//! these items out of an object as it is written and returns them as the
//! writer's layer; [`merge`] puts a reader's own layer back as the object is
//! served. Whole source lines move, so `merge(split(x)) == x` byte for byte.

use serde::{Deserialize, Serialize};

use crate::lines::{Component, Document, Item, Line};

/// Properties that hold one user's view of an event (Thunderbird keeps alarm
/// acknowledgement and snooze state on the event itself).
const PER_USER_PROPERTIES: [&str; 2] = ["X-MOZ-LASTACK", "X-MOZ-SNOOZE-TIME"];
/// Thunderbird writes one per snoozed occurrence: `X-MOZ-SNOOZE-TIME-<time>`.
const PER_USER_PREFIXES: [&str; 1] = ["X-MOZ-SNOOZE-TIME-"];
const PER_USER_COMPONENTS: [&str; 1] = ["VALARM"];

/// One user's personal items for one object.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UserLayer {
    pub entries: Vec<LayerEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LayerEntry {
    /// The component it belongs to: its UID ...
    pub uid: String,
    /// ... and its RECURRENCE-ID line, for an override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recurrence_id: Option<String>,
    /// Its position among the component's items when it was taken out.
    pub index: usize,
    /// Its exact source text.
    pub source: String,
}

fn is_personal(item: &Item) -> bool {
    match item {
        Item::Component(c) => PER_USER_COMPONENTS.contains(&c.name.as_str()),
        Item::Property(l) => {
            let n = l.name();
            PER_USER_PROPERTIES.contains(&n.as_str()) || PER_USER_PREFIXES.iter().any(|p| n.starts_with(p))
        }
    }
}

/// The components of a calendar object that carry a UID.
fn owners(doc: &mut Document) -> impl Iterator<Item = &mut Component> {
    doc.components.iter_mut().filter(|c| c.name == "VCALENDAR").flat_map(|cal| cal.items.iter_mut()).filter_map(|i| {
        match i {
            Item::Component(c) if c.property("UID").is_some() => Some(c),
            _ => None,
        }
    })
}

fn key(c: &Component) -> (String, Option<String>) {
    (
        c.property("UID").map(|l| l.value().to_string()).unwrap_or_default(),
        c.property("RECURRENCE-ID").map(|l| l.text().to_string()),
    )
}

/// Take the personal items out of `doc` and return them.
pub fn split(doc: &mut Document) -> UserLayer {
    let mut layer = UserLayer::default();
    for comp in owners(doc) {
        let (uid, recurrence_id) = key(comp);
        let items = std::mem::take(&mut comp.items);
        for (index, item) in items.into_iter().enumerate() {
            if is_personal(&item) {
                let source = match &item {
                    Item::Component(c) => c.to_string(),
                    Item::Property(l) => l.source(),
                };
                layer.entries.push(LayerEntry {
                    uid: uid.clone(),
                    recurrence_id: recurrence_id.clone(),
                    index,
                    source,
                });
            } else {
                comp.items.push(item);
            }
        }
    }
    layer
}

/// Put a user's personal items back into `doc`. Entries for components that
/// no longer exist are dropped; positions past the end land at the end.
pub fn merge(doc: &mut Document, layer: &UserLayer) {
    for comp in owners(doc) {
        let (uid, recurrence_id) = key(comp);
        let mut mine: Vec<&LayerEntry> =
            layer.entries.iter().filter(|e| e.uid == uid && e.recurrence_id == recurrence_id).collect();
        mine.sort_by_key(|e| e.index);
        for e in mine {
            let Some(item) = restore(&e.source) else { continue };
            let at = e.index.min(comp.items.len());
            comp.items.insert(at, item);
        }
    }
}

fn restore(source: &str) -> Option<Item> {
    if source.get(..6).is_some_and(|p| p.eq_ignore_ascii_case("BEGIN:")) {
        let mut doc = Document::parse(source).ok()?;
        (doc.components.len() == 1).then(|| Item::Component(doc.components.remove(0)))
    } else {
        Line::from_source(source).map(Item::Property)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const THUNDERBIRD: &str = "BEGIN:VCALENDAR\r\nPRODID:-//Mozilla.org/NONSGML Mozilla Calendar V1.1//EN\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nCREATED:20261101T100000Z\r\nUID:tb-1\r\nSUMMARY:Synthetic\r\nX-MOZ-LASTACK:20261105T080000Z\r\nDTSTART:20261106T090000Z\r\nDTEND:20261106T100000Z\r\nX-MOZ-SNOOZE-TIME:20261106T085500Z\r\nX-MOZ-GENERATION:3\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:Default Mozilla Description\r\nEND:VALARM\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:tb-1\r\nRECURRENCE-ID:20261107T090000Z\r\nDTSTART:20261107T110000Z\r\nDTEND:20261107T120000Z\r\nX-MOZ-SNOOZE-TIME-1794042000000000:20261107T105500Z\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT5M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn split_then_merge_is_byte_identical() {
        let mut doc = Document::parse(THUNDERBIRD).unwrap();
        let layer = split(&mut doc);
        assert_eq!(layer.entries.len(), 5);
        let shared = doc.to_string();
        assert!(!shared.contains("VALARM") && !shared.contains("X-MOZ-LASTACK") && !shared.contains("X-MOZ-SNOOZE"));
        assert!(shared.contains("X-MOZ-GENERATION:3"), "shared Thunderbird state stays");
        merge(&mut doc, &layer);
        assert_eq!(doc.to_string(), THUNDERBIRD);
    }

    #[test]
    fn another_users_layer_replaces_mine() {
        let mut doc = Document::parse(THUNDERBIRD).unwrap();
        let _mine = split(&mut doc);
        let theirs = UserLayer {
            entries: vec![LayerEntry {
                uid: "tb-1".into(),
                recurrence_id: None,
                index: 99,
                source: "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT1H\r\nEND:VALARM\r\n".into(),
            }],
        };
        merge(&mut doc, &theirs);
        let served = doc.to_string();
        assert!(served.contains("TRIGGER:-PT1H"));
        assert!(!served.contains("TRIGGER:-PT15M") && !served.contains("TRIGGER:-PT5M"));
    }

    #[test]
    fn entries_for_vanished_components_are_dropped() {
        let mut doc = Document::parse(THUNDERBIRD).unwrap();
        let layer = split(&mut doc);
        let mut other = Document::parse(&THUNDERBIRD.replace("UID:tb-1", "UID:tb-2")).unwrap();
        let _ = split(&mut other);
        let before = other.to_string();
        merge(&mut other, &layer);
        assert_eq!(other.to_string(), before);
    }

    #[test]
    fn layers_survive_json() {
        let mut doc = Document::parse(THUNDERBIRD).unwrap();
        let layer = split(&mut doc);
        let back: UserLayer = serde_json::from_str(&serde_json::to_string(&layer).unwrap()).unwrap();
        assert_eq!(back, layer);
    }
}
