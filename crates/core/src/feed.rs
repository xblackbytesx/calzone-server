//! Published feeds (decision 24): a calendar as one `.ics` for a secret
//! link, at full or busy-only detail. Personal items (alarms and their
//! client state) are never in stored shared bodies, so never in a feed.
//!
//! - **Busy-only** keeps when an event blocks time and nothing else: the
//!   times, recurrence and identity lines. Events marked free
//!   (`TRANSP:TRANSPARENT`) and canceled ones are left out, and tasks and
//!   journal entries are never published.
//! - **Full** publishes events as they are, except that an event marked
//!   `CLASS:PRIVATE` or `CLASS:CONFIDENTIAL` is published as busy-only.

use crate::import::assemble_calendar;
use crate::lines::{Component, Document, Item, Line};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detail {
    Full,
    Busy,
}

/// The lines a busy-only event keeps.
const BUSY_KEEP: [&str; 11] = [
    "UID",
    "DTSTAMP",
    "DTSTART",
    "DTEND",
    "DURATION",
    "RRULE",
    "RDATE",
    "EXDATE",
    "RECURRENCE-ID",
    "SEQUENCE",
    "STATUS",
];

fn is_private(c: &Component) -> bool {
    c.property("CLASS")
        .is_some_and(|l| matches!(l.value().trim().to_ascii_uppercase().as_str(), "PRIVATE" | "CONFIDENTIAL"))
}

fn blocks_time(c: &Component) -> bool {
    let transparent = c.property("TRANSP").is_some_and(|l| l.value().trim().eq_ignore_ascii_case("TRANSPARENT"));
    let canceled = c.property("STATUS").is_some_and(|l| l.value().trim().eq_ignore_ascii_case("CANCELLED"));
    !transparent && !canceled
}

/// One event component as busy-only: identity and times, and a plain title.
fn busy_component(c: &Component, title: &str) -> Component {
    let mut out = Component { items: Vec::new(), ..c.clone() };
    for item in &c.items {
        if let Item::Property(l) = item
            && BUSY_KEEP.contains(&l.name().as_str())
        {
            out.items.push(item.clone());
        }
    }
    out.items.push(Item::Property(Line::new(format!("SUMMARY:{}", crate::edit::escape_text(title)))));
    out
}

/// One stored object as it goes into a feed; `None` leaves it out.
/// `busy_title` is the title busy entries get ("Busy").
pub fn object_for_feed(text: &str, detail: Detail, busy_title: &str) -> Option<String> {
    let mut doc = Document::parse(text).ok()?;
    let cal = doc.components.first_mut().filter(|c| c.name == "VCALENDAR")?;
    // Events only: tasks and journal entries are not published.
    if !cal.components().any(|c| c.name == "VEVENT") {
        return None;
    }
    let series_private = cal.components().filter(|c| c.name == "VEVENT").any(is_private);
    let busy = detail == Detail::Busy || series_private;
    let mut items = Vec::new();
    for item in std::mem::take(&mut cal.items) {
        match item {
            Item::Component(c) if c.name == "VEVENT" => {
                if busy {
                    // An override is always kept: dropping it would bring
                    // back the original occurrence it replaced.
                    if blocks_time(&c) || c.property("RECURRENCE-ID").is_some() {
                        items.push(Item::Component(busy_component(&c, busy_title)));
                    }
                } else {
                    items.push(Item::Component(c));
                }
            }
            Item::Component(c) if c.name == "VTIMEZONE" => items.push(Item::Component(c)),
            Item::Component(_) => {}
            Item::Property(l) => items.push(Item::Property(l)),
        }
    }
    // A series that does not block time at all is left out in busy mode.
    let any_event = items.iter().any(|i| matches!(i, Item::Component(c) if c.name == "VEVENT"));
    if !any_event {
        return None;
    }
    cal.items = items;
    Some(doc.to_string())
}

/// A whole feed: every object of a calendar at the given detail.
pub fn publish(name: &str, objects: &[String], detail: Detail, busy_title: &str) -> String {
    let parts: Vec<String> = objects.iter().filter_map(|o| object_for_feed(o, detail, busy_title)).collect();
    assemble_calendar(name, &parts).0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\n{body}END:VCALENDAR\r\n")
    }

    fn event(extra: &str) -> String {
        cal(&format!(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20260101T000000Z\r\nDTSTART:20260105T090000Z\r\nDTEND:20260105T100000Z\r\n\
             RRULE:FREQ=WEEKLY\r\nSUMMARY:Therapy\r\nLOCATION:Clinic\r\nDESCRIPTION:Room 4\r\nATTENDEE:mailto:x@example.org\r\n{extra}END:VEVENT\r\n"
        ))
    }

    #[test]
    fn busy_keeps_times_and_nothing_else() {
        let out = object_for_feed(&event(""), Detail::Busy, "Busy").unwrap();
        for keep in ["UID:a", "DTSTART:20260105T090000Z", "DTEND:20260105T100000Z", "RRULE:FREQ=WEEKLY", "SUMMARY:Busy"]
        {
            assert!(out.contains(keep), "{keep}");
        }
        for gone in ["Therapy", "Clinic", "Room 4", "ATTENDEE"] {
            assert!(!out.contains(gone), "{gone}");
        }
        // Still a valid calendar object with the same occurrences.
        let doc = Document::parse(&out).unwrap();
        crate::object::inspect_calendar(&doc, &out).unwrap();
    }

    #[test]
    fn full_publishes_except_private_ones() {
        let full = object_for_feed(&event(""), Detail::Full, "Busy").unwrap();
        assert!(full.contains("SUMMARY:Therapy") && full.contains("LOCATION:Clinic"));
        let private = object_for_feed(&event("CLASS:PRIVATE\r\n"), Detail::Full, "Bezet").unwrap();
        assert!(private.contains("SUMMARY:Bezet") && !private.contains("Therapy") && !private.contains("CLASS"));
        let conf = object_for_feed(&event("CLASS:confidential\r\n"), Detail::Full, "Busy").unwrap();
        assert!(!conf.contains("Clinic"));
    }

    #[test]
    fn free_canceled_tasks_and_layers_stay_out() {
        assert_eq!(object_for_feed(&event("TRANSP:TRANSPARENT\r\n"), Detail::Busy, "Busy"), None);
        assert_eq!(object_for_feed(&event("STATUS:CANCELLED\r\n"), Detail::Busy, "Busy"), None);
        assert!(object_for_feed(&event("TRANSP:TRANSPARENT\r\n"), Detail::Full, "Busy").is_some(), "full shows it");
        let todo = cal("BEGIN:VTODO\r\nUID:t\r\nSUMMARY:Groceries\r\nEND:VTODO\r\n");
        assert_eq!(object_for_feed(&todo, Detail::Full, "Busy"), None);
        let feed = publish(
            "Family",
            &[event(""), todo, event("CLASS:PRIVATE\r\n").replace("UID:a", "UID:b")],
            Detail::Full,
            "Busy",
        );
        assert_eq!(feed.matches("BEGIN:VEVENT").count(), 2);
        assert!(feed.contains("X-WR-CALNAME:Family") && !feed.contains("Groceries"));
        assert!(!feed.contains("VALARM"));
    }
}
