//! The three-way merge of one stored object (decision 15).
//!
//! A phone sends its edit together with the copy it started from (the
//! base); the server holds the current copy (theirs). [`merge`] combines the
//! two edits field by field:
//!
//! - A field only one side changed takes that side's value, so edits to
//!   different fields both survive.
//! - A field both sides changed to different values keeps the server's
//!   value, and the phone's value comes back as a [`Conflict`] to show the
//!   person.
//! - Each occurrence of a series (UID plus RECURRENCE-ID) is its own unit,
//!   and so is each VTIMEZONE (by TZID).
//! - Repeatable properties (attendees, EXDATE, phone numbers, emails,
//!   unknown properties) merge as sets: additions and removals from both
//!   sides are all applied. Components without an identity (VALARM, a
//!   VTIMEZONE's rules, unknown ones) merge the same way by their text.
//!
//! Nothing is dropped for not being understood: a property, parameter or
//! component either side has comes through. Lines that are kept keep their
//! exact source bytes. Pure: no IO, no clock.

use std::collections::{HashMap, HashSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::lines::{Component, Document, Item, Line, ParseError};

/// The result of a merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Merged {
    /// The merged text, or `None` when the object ends up deleted.
    pub text: Option<String>,
    /// What the phone's side lost, to be reported to the person.
    pub conflicts: Vec<Conflict>,
}

/// One thing the phone's side lost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Conflict {
    /// The component it happened in (`VEVENT`, `VCARD`, `VCALENDAR`, ...).
    pub component: String,
    /// That component's UID, if it has one.
    pub uid: Option<String>,
    /// That component's RECURRENCE-ID value, for one occurrence of a series.
    pub recurrence_id: Option<String>,
    pub lost: Lost,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Lost {
    /// Both sides changed this field: the server's lines are kept, the
    /// phone's are lost. `field` is a property name, or [`TIME`] for the
    /// start, end, duration and due date together.
    Field { field: String, kept: Vec<String>, lost: Vec<String> },
    /// The phone deleted this while the server changed it: the server's
    /// copy is kept.
    Delete,
    /// The server deleted this while the phone changed it: the phone's copy
    /// (`lost`, its text) is gone.
    Edit { lost: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Side {
    Base,
    Mine,
    Theirs,
}

impl fmt::Display for Side {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Side::Base => "the base copy",
            Side::Mine => "the phone's copy",
            Side::Theirs => "the server's copy",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MergeError {
    #[error("{side} does not parse: {error}")]
    Parse { side: Side, error: ParseError },
    #[error("{0} is not exactly one top-level component")]
    NotOne(Side),
    #[error("the copies are different kinds of object")]
    Mismatch,
}

/// The field name for DTSTART, DTEND, DURATION and DUE, which merge as one:
/// a moved event moves its start and end together.
pub const TIME: &str = "TIME";
const TIME_PROPERTIES: [&str; 4] = ["DTSTART", "DTEND", "DURATION", "DUE"];

/// Properties that hold one value in iCalendar (RFC 5545), besides [`TIME`].
const CALENDAR_SINGLE: [&str; 22] = [
    "SUMMARY",
    "DESCRIPTION",
    "LOCATION",
    "RRULE",
    "STATUS",
    "CLASS",
    "TRANSP",
    "PRIORITY",
    "ORGANIZER",
    "URL",
    "GEO",
    "UID",
    "RECURRENCE-ID",
    "COLOR",
    "PERCENT-COMPLETE",
    "COMPLETED",
    "VERSION",
    "CALSCALE",
    "TZID",
    "TZURL",
    "TZOFFSETFROM",
    "TZOFFSETTO",
];

/// Properties that hold one value in a vCard (RFC 6350, and in practice).
const CARD_SINGLE: [&str; 12] =
    ["FN", "N", "BDAY", "ANNIVERSARY", "GENDER", "KIND", "UID", "VERSION", "ORG", "TITLE", "ROLE", "NOTE"];

/// Bookkeeping clients rewrite on every save. When both sides changed one,
/// nothing is reported: SEQUENCE takes the higher number, the rest the
/// server's line.
const BOOKKEEPING: [&str; 6] = ["DTSTAMP", "LAST-MODIFIED", "CREATED", "PRODID", "REV", "SEQUENCE"];

/// Merge the phone's copy (`mine`) and the server's (`theirs`) of one
/// object, both edited from `base`. `None` is "absent": not created yet, or
/// deleted. Each present copy must be one top-level component (one
/// `VCALENDAR` or one `VCARD`).
pub fn merge(base: Option<&str>, mine: Option<&str>, theirs: Option<&str>) -> Result<Merged, MergeError> {
    let done = |text: Option<&str>| Ok(Merged { text: text.map(str::to_string), conflicts: Vec::new() });
    match (mine, theirs) {
        (None, None) => done(None),
        // The server deleted it.
        (Some(m), None) => match base {
            None => done(Some(m)), // new on the phone
            Some(b) if b == m => done(None),
            Some(_) => {
                let doc = parse(m, Side::Mine)?;
                let (component, uid, recurrence_id) = identity(main(&doc.components[0]));
                Ok(Merged {
                    text: None,
                    conflicts: vec![Conflict {
                        component,
                        uid,
                        recurrence_id,
                        lost: Lost::Edit { lost: m.to_string() },
                    }],
                })
            }
        },
        // The phone deleted it.
        (None, Some(t)) => match base {
            None => done(Some(t)), // the phone never had it
            Some(b) if b == t => done(None),
            Some(_) => {
                let doc = parse(t, Side::Theirs)?;
                let (component, uid, recurrence_id) = identity(main(&doc.components[0]));
                Ok(Merged {
                    text: Some(t.to_string()),
                    conflicts: vec![Conflict { component, uid, recurrence_id, lost: Lost::Delete }],
                })
            }
        },
        (Some(m), Some(t)) => {
            if m == t || base == Some(m) {
                return done(Some(t));
            }
            if base == Some(t) {
                return done(Some(m));
            }
            let m_doc = parse(m, Side::Mine)?;
            let t_doc = parse(t, Side::Theirs)?;
            let b_doc = base.map(|b| parse(b, Side::Base)).transpose()?;
            let (mc, tc) = (&m_doc.components[0], &t_doc.components[0]);
            if mc.name != tc.name || b_doc.as_ref().is_some_and(|b| b.components[0].name != tc.name) {
                return Err(MergeError::Mismatch);
            }
            let mut conflicts = Vec::new();
            let merged = component(b_doc.as_ref().map(|b| &b.components[0]), mc, tc, &mut conflicts);
            let doc = Document { components: vec![merged], trailing: t_doc.trailing.clone() };
            Ok(Merged { text: Some(doc.to_string()), conflicts })
        }
    }
}

fn parse(text: &str, side: Side) -> Result<Document, MergeError> {
    let doc = Document::parse(text).map_err(|error| MergeError::Parse { side, error })?;
    if doc.components.len() != 1 {
        return Err(MergeError::NotOne(side));
    }
    Ok(doc)
}

/// The component a whole-object conflict is about: a calendar object's
/// master (or its first component with a UID), or the card itself.
fn main(top: &Component) -> &Component {
    let owners: Vec<&Component> = top.components().filter(|c| c.property("UID").is_some()).collect();
    owners.iter().find(|c| c.property("RECURRENCE-ID").is_none()).or(owners.first()).copied().unwrap_or(top)
}

fn identity(c: &Component) -> (String, Option<String>, Option<String>) {
    let value = |name: &str| c.property(name).map(|l| l.value().to_string());
    (c.name.clone(), value("UID"), value("RECURRENCE-ID"))
}

fn text(c: &Component) -> String {
    c.to_string()
}

fn field(line: &Line) -> String {
    let name = line.name();
    if TIME_PROPERTIES.contains(&name.as_str()) { TIME.to_string() } else { name }
}

fn single(component: &str, field: &str) -> bool {
    if component == "VCARD" {
        CARD_SINGLE.contains(&field)
    } else {
        field == TIME || CALENDAR_SINGLE.contains(&field) || BOOKKEEPING.contains(&field)
    }
}

/// A sub-component's identity among its siblings. Components with a UID go
/// by UID and RECURRENCE-ID, VTIMEZONEs by TZID; the rest by their text (and
/// the how-many-th copy of that text), so an edit there is a removal plus an
/// addition and both sides' versions survive.
fn component_keys(c: Option<&Component>) -> Vec<(String, &Component)> {
    let mut seen: HashMap<String, usize> = HashMap::new();
    c.into_iter()
        .flat_map(|c| c.components())
        .map(|sub| {
            let value = |name: &str| sub.property(name).map(|l| l.value().to_string());
            let key = if let Some(uid) = value("UID") {
                format!("{}\0uid\0{uid}\0{}", sub.name, value("RECURRENCE-ID").unwrap_or_default())
            } else if let (true, Some(tzid)) = (sub.name == "VTIMEZONE", value("TZID")) {
                format!("{}\0tzid\0{tzid}", sub.name)
            } else {
                let body = text(sub);
                let n = seen.entry(body.clone()).or_insert(0);
                *n += 1;
                format!("{}\0text\0{}\0{body}", sub.name, n)
            };
            (key, sub)
        })
        .collect()
}

fn find<'a>(keys: &[(String, &'a Component)], k: &str) -> Option<&'a Component> {
    keys.iter().find(|(key, _)| key == k).map(|(_, c)| *c)
}

fn component(base: Option<&Component>, mine: &Component, theirs: &Component, out: &mut Vec<Conflict>) -> Component {
    let (m_text, t_text) = (text(mine), text(theirs));
    if m_text == t_text {
        return theirs.clone();
    }
    if let Some(b) = base {
        let b_text = text(b);
        if b_text == m_text {
            return theirs.clone();
        }
        if b_text == t_text {
            return mine.clone();
        }
    }
    let (name, uid, recurrence_id) = identity(theirs);
    let conflict =
        |lost: Lost| Conflict { component: name.clone(), uid: uid.clone(), recurrence_id: recurrence_id.clone(), lost };

    // Properties, field by field.
    let lines = |c: Option<&Component>, f: &str| -> Vec<Line> {
        c.into_iter().flat_map(|c| c.properties()).filter(|l| field(l) == f).cloned().collect()
    };
    let mut fields: Vec<String> = Vec::new();
    for c in [Some(theirs), Some(mine), base].into_iter().flatten() {
        for l in c.properties() {
            let f = field(l);
            if !fields.contains(&f) {
                fields.push(f);
            }
        }
    }
    let mut decided: HashMap<String, Vec<Line>> = HashMap::new();
    for f in &fields {
        let (b, m, t) = (lines(base, f), lines(Some(mine), f), lines(Some(theirs), f));
        let texts = |v: &[Line]| v.iter().map(|l| l.text().to_string()).collect::<Vec<_>>();
        let (bx, mx, tx) = (texts(&b), texts(&m), texts(&t));
        let result = if mx == tx || bx == mx {
            t
        } else if bx == tx {
            m
        } else if single(&theirs.name, f) {
            if f == "SEQUENCE" {
                let number = |v: &[Line]| v.first().and_then(|l| l.value().trim().parse::<i64>().ok()).unwrap_or(0);
                if number(&m) > number(&t) { m } else { t }
            } else {
                if !BOOKKEEPING.contains(&f.as_str()) {
                    out.push(conflict(Lost::Field { field: f.clone(), kept: tx, lost: mx }));
                }
                t
            }
        } else {
            set_merge(&b, &m, &t)
        };
        decided.insert(f.clone(), result);
    }

    // Sub-components, unit by unit.
    let (b_keys, m_keys, t_keys) = (component_keys(base), component_keys(Some(mine)), component_keys(Some(theirs)));
    let mut merged_subs: HashMap<String, Option<Component>> = HashMap::new();
    for (k, t) in &t_keys {
        let result = match (find(&m_keys, k), find(&b_keys, k)) {
            (Some(m), b) => Some(component(b, m, t, out)),
            (None, None) => Some((*t).clone()), // added on the server
            (None, Some(b)) => {
                if text(b) == text(t) {
                    None // deleted on the phone
                } else {
                    let (component, uid, recurrence_id) = identity(t);
                    out.push(Conflict { component, uid, recurrence_id, lost: Lost::Delete });
                    Some((*t).clone())
                }
            }
        };
        merged_subs.insert(k.clone(), result);
    }
    let mut added_subs: Vec<Component> = Vec::new();
    for (k, m) in &m_keys {
        if find(&t_keys, k).is_some() {
            continue;
        }
        match find(&b_keys, k) {
            None => added_subs.push((*m).clone()), // added on the phone
            Some(b) if text(b) == text(m) => {}    // deleted on the server
            Some(_) => {
                let (component, uid, recurrence_id) = identity(m);
                out.push(Conflict { component, uid, recurrence_id, lost: Lost::Edit { lost: text(m) } });
            }
        }
    }

    // Lay it out in the server's order; the phone's new fields go after the
    // server's properties, its new components at the end.
    let mut items: Vec<Item> = Vec::new();
    let mut emitted: HashSet<String> = HashSet::new();
    let mut t_subs = t_keys.iter();
    for item in &theirs.items {
        match item {
            Item::Property(l) => {
                let f = field(l);
                if emitted.insert(f.clone()) {
                    items.extend(decided[&f].iter().cloned().map(Item::Property));
                }
            }
            Item::Component(_) => {
                let (k, _) = t_subs.next().expect("one key per sub-component");
                if let Some(c) = merged_subs.remove(k).flatten() {
                    items.push(Item::Component(c));
                }
            }
        }
    }
    let new_fields: Vec<Item> = fields
        .iter()
        .filter(|f| !emitted.contains(*f))
        .flat_map(|f| decided[f].iter().cloned().map(Item::Property))
        .collect();
    let at = items.iter().position(|i| matches!(i, Item::Component(_))).unwrap_or(items.len());
    items.splice(at..at, new_fields);
    items.extend(added_subs.into_iter().map(Item::Component));

    Component { name: theirs.name.clone(), begin: theirs.begin.clone(), items, end: theirs.end.clone() }
}

/// Both sides' additions and removals on a repeatable property. Lines are
/// compared by their unfolded text, and counted, so a line that appears
/// twice is removed once per removal.
fn set_merge(base: &[Line], mine: &[Line], theirs: &[Line]) -> Vec<Line> {
    fn counts(lines: &[Line]) -> HashMap<&str, usize> {
        let mut m = HashMap::new();
        for l in lines {
            *m.entry(l.text()).or_insert(0) += 1;
        }
        m
    }
    let (b, m, t) = (counts(base), counts(mine), counts(theirs));
    let mut removed: HashMap<&str, usize> =
        b.iter().map(|(k, n)| (*k, n.saturating_sub(m.get(k).copied().unwrap_or(0)))).collect();
    let mut out: Vec<Line> = Vec::new();
    for l in theirs {
        if let Some(n) = removed.get_mut(l.text()).filter(|n| **n > 0) {
            *n -= 1;
            continue;
        }
        out.push(l.clone());
    }
    let mut added: HashMap<&str, usize> = m
        .iter()
        .map(|(k, n)| {
            let had = b.get(k).copied().unwrap_or(0).max(t.get(k).copied().unwrap_or(0));
            (*k, n.saturating_sub(had))
        })
        .collect();
    for l in mine {
        if let Some(n) = added.get_mut(l.text()).filter(|n| **n > 0) {
            *n -= 1;
            out.push(l.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\nBEGIN:VEVENT\r\nUID:e1\r\nDTSTAMP:20261101T100000Z\r\nSEQUENCE:1\r\nSUMMARY:Swimming\r\nLOCATION:Pool\r\nDTSTART;TZID=Europe/Amsterdam:20261116T160000\r\nDTEND;TZID=Europe/Amsterdam:20261116T170000\r\nRRULE:FREQ=WEEKLY;COUNT=6\r\nEXDATE;TZID=Europe/Amsterdam:20261130T160000\r\nX-UNKNOWN;X-P=1:kept\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    fn edit(text: &str, from: &str, to: &str) -> String {
        assert_eq!(text.matches(from).count(), 1, "anchor {from:?}");
        text.replacen(from, to, 1)
    }

    fn run(base: Option<&str>, mine: Option<&str>, theirs: Option<&str>) -> Merged {
        merge(base, mine, theirs).unwrap()
    }

    fn lines_of(text: &str) -> Vec<String> {
        text.split("\r\n").map(str::to_string).collect()
    }

    #[test]
    fn an_unchanged_side_gives_the_other_side_exactly() {
        let changed = edit(BASE, "SUMMARY:Swimming", "SUMMARY:Swim\r\n  lessons"); // folded oddly on purpose
        for (m, t, want) in [(BASE, changed.as_str(), changed.as_str()), (changed.as_str(), BASE, changed.as_str())] {
            let r = run(Some(BASE), Some(m), Some(t));
            assert_eq!(r.text.as_deref(), Some(want));
            assert!(r.conflicts.is_empty());
        }
    }

    #[test]
    fn edits_to_different_fields_both_survive() {
        let mine = edit(BASE, "SUMMARY:Swimming", "SUMMARY:Swimming lessons");
        let theirs = edit(BASE, "LOCATION:Pool", "LOCATION:New pool");
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(text.contains("SUMMARY:Swimming lessons\r\n") && text.contains("LOCATION:New pool\r\n"));
        assert!(r.conflicts.is_empty());
        // Everything else is the base, in the base's order.
        assert_eq!(text, edit(&theirs, "SUMMARY:Swimming", "SUMMARY:Swimming lessons"));
    }

    #[test]
    fn the_same_field_keeps_the_server_value_and_reports_the_phones() {
        let mine = edit(BASE, "SUMMARY:Swimming", "SUMMARY:Mine");
        let theirs = edit(BASE, "SUMMARY:Swimming", "SUMMARY:Theirs");
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        assert_eq!(r.text.as_deref(), Some(theirs.as_str()));
        assert_eq!(
            r.conflicts,
            vec![Conflict {
                component: "VEVENT".into(),
                uid: Some("e1".into()),
                recurrence_id: None,
                lost: Lost::Field {
                    field: "SUMMARY".into(),
                    kept: vec!["SUMMARY:Theirs".into()],
                    lost: vec!["SUMMARY:Mine".into()]
                },
            }]
        );
    }

    #[test]
    fn the_same_change_on_both_sides_is_no_conflict() {
        let both = edit(BASE, "SUMMARY:Swimming", "SUMMARY:Same");
        let mine = edit(&both, "LOCATION:Pool", "LOCATION:Other");
        let r = run(Some(BASE), Some(&mine), Some(&both));
        assert!(r.conflicts.is_empty());
        assert!(r.text.unwrap().contains("SUMMARY:Same\r\nLOCATION:Other\r\n"));
    }

    #[test]
    fn start_and_end_move_together() {
        let mine = edit(
            &edit(BASE, "20261116T160000\r\nDTEND", "20261116T180000\r\nDTEND"),
            "DTEND;TZID=Europe/Amsterdam:20261116T170000",
            "DTEND;TZID=Europe/Amsterdam:20261116T190000",
        );
        let theirs = edit(BASE, "DTEND;TZID=Europe/Amsterdam:20261116T170000", "DURATION:PT2H");
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        // The server's time is kept whole, never its start with the phone's end.
        assert!(text.contains("DTSTART;TZID=Europe/Amsterdam:20261116T160000\r\nDURATION:PT2H\r\n"), "{text}");
        assert!(!text.contains("DTEND"));
        assert!(matches!(&r.conflicts[0].lost, Lost::Field { field, lost, .. } if field == TIME && lost.len() == 2));
    }

    #[test]
    fn repeatable_and_unknown_properties_merge_as_sets() {
        let mine = edit(
            &edit(BASE, "X-UNKNOWN;X-P=1:kept", "X-UNKNOWN;X-P=1:kept\r\nX-MINE:1"),
            "EXDATE;TZID=Europe/Amsterdam:20261130T160000",
            "EXDATE;TZID=Europe/Amsterdam:20261130T160000\r\nEXDATE;TZID=Europe/Amsterdam:20261207T160000",
        );
        let theirs = edit(
            &edit(BASE, "X-UNKNOWN;X-P=1:kept", "X-UNKNOWN;X-P=1:kept\r\nX-THEIRS;X-Q=\"a,b\":2"),
            "EXDATE;TZID=Europe/Amsterdam:20261130T160000",
            "EXDATE;TZID=Europe/Amsterdam:20261130T160000\r\nEXDATE;TZID=Europe/Amsterdam:20261214T160000",
        );
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        for want in [
            "X-MINE:1",
            "X-THEIRS;X-Q=\"a,b\":2",
            "X-UNKNOWN;X-P=1:kept",
            "20261207T160000",
            "20261214T160000",
            "20261130T160000",
        ] {
            assert!(text.contains(want), "{want} missing");
        }
        assert!(r.conflicts.is_empty());
        // A removal on one side and an addition on the other both apply.
        let mine = edit(BASE, "EXDATE;TZID=Europe/Amsterdam:20261130T160000\r\n", "");
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(!text.contains("20261130T160000") && text.contains("20261214T160000"));
    }

    #[test]
    fn one_occurrence_is_its_own_unit() {
        let exception = "BEGIN:VEVENT\r\nUID:e1\r\nRECURRENCE-ID;TZID=Europe/Amsterdam:20261123T160000\r\nSUMMARY:Swimming\r\nDTSTART;TZID=Europe/Amsterdam:20261123T180000\r\nDTEND;TZID=Europe/Amsterdam:20261123T190000\r\nEND:VEVENT\r\n";
        // The phone moves one occurrence; the web renames the series.
        let mine = edit(BASE, "END:VEVENT\r\nEND:VCALENDAR", &format!("END:VEVENT\r\n{exception}END:VCALENDAR"));
        let theirs = edit(BASE, "SUMMARY:Swimming", "SUMMARY:Swimming lessons");
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(text.contains("SUMMARY:Swimming lessons\r\n") && text.contains(exception));
        assert!(r.conflicts.is_empty());
        // Then both edit that occurrence, different fields.
        let base = text;
        let mine = edit(
            &base,
            "DTSTART;TZID=Europe/Amsterdam:20261123T180000",
            "DTSTART;TZID=Europe/Amsterdam:20261123T173000",
        );
        let theirs = edit(
            &base,
            "RECURRENCE-ID;TZID=Europe/Amsterdam:20261123T160000\r\nSUMMARY:Swimming",
            "RECURRENCE-ID;TZID=Europe/Amsterdam:20261123T160000\r\nSUMMARY:Gala",
        );
        let r = run(Some(&base), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(text.contains("SUMMARY:Gala\r\nDTSTART;TZID=Europe/Amsterdam:20261123T173000\r\n"), "{text}");
        assert!(r.conflicts.is_empty());
    }

    #[test]
    fn deletes_against_edits() {
        let changed = edit(BASE, "SUMMARY:Swimming", "SUMMARY:Changed");
        // Deleted on the phone, untouched on the server: deleted.
        assert_eq!(run(Some(BASE), None, Some(BASE)).text, None);
        // Deleted on the phone, changed on the server: the server's copy stays.
        let r = run(Some(BASE), None, Some(&changed));
        assert_eq!(r.text.as_deref(), Some(changed.as_str()));
        assert_eq!(r.conflicts[0].lost, Lost::Delete);
        assert_eq!(r.conflicts[0].uid.as_deref(), Some("e1"));
        // Deleted on the server, untouched on the phone: deleted.
        assert_eq!(run(Some(BASE), Some(BASE), None).text, None);
        // Deleted on the server, changed on the phone: stays deleted, reported.
        let r = run(Some(BASE), Some(&changed), None);
        assert_eq!(r.text, None);
        assert_eq!(r.conflicts[0].lost, Lost::Edit { lost: changed.clone() });
        // New on the phone.
        assert_eq!(run(None, Some(BASE), None).text.as_deref(), Some(BASE));
        // An occurrence the phone removed while the server changed it stays.
        let exception = "BEGIN:VEVENT\r\nUID:e1\r\nRECURRENCE-ID:20261123T150000Z\r\nSUMMARY:One\r\nEND:VEVENT\r\n";
        let with = edit(BASE, "END:VEVENT\r\nEND:VCALENDAR", &format!("END:VEVENT\r\n{exception}END:VCALENDAR"));
        let theirs = edit(&with, "SUMMARY:One", "SUMMARY:Two");
        let mine = edit(&with, "SUMMARY:Swimming", "SUMMARY:Renamed").replace(exception, "");
        let r = run(Some(&with), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(text.contains("SUMMARY:Two") && text.contains("SUMMARY:Renamed"));
        assert_eq!(r.conflicts.len(), 1);
        assert_eq!(r.conflicts[0].recurrence_id.as_deref(), Some("20261123T150000Z"));
        assert_eq!(r.conflicts[0].lost, Lost::Delete);
    }

    #[test]
    fn alarms_and_unknown_components_from_both_sides_survive() {
        let alarm = |t: &str| format!("BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:{t}\r\nEND:VALARM\r\n");
        let mine = edit(BASE, "END:VEVENT", &format!("{}END:VEVENT", alarm("-PT15M")));
        let theirs =
            edit(BASE, "END:VEVENT", &format!("{}BEGIN:X-THING\r\nA:1\r\nEND:X-THING\r\nEND:VEVENT", alarm("-PT1H")));
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(text.contains(&alarm("-PT15M")) && text.contains(&alarm("-PT1H")) && text.contains("BEGIN:X-THING"));
        assert!(r.conflicts.is_empty());
    }

    #[test]
    fn bookkeeping_is_never_a_conflict() {
        let mine =
            edit(&edit(BASE, "SEQUENCE:1", "SEQUENCE:3"), "DTSTAMP:20261101T100000Z", "DTSTAMP:20261102T100000Z");
        let mine = edit(&mine, "SUMMARY:Swimming", "SUMMARY:A");
        let theirs =
            edit(&edit(BASE, "SEQUENCE:1", "SEQUENCE:2"), "DTSTAMP:20261101T100000Z", "DTSTAMP:20261103T100000Z");
        let theirs = edit(&theirs, "LOCATION:Pool", "LOCATION:B");
        let r = run(Some(BASE), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(text.contains("SEQUENCE:3\r\n") && text.contains("DTSTAMP:20261103T100000Z\r\n"));
        assert!(r.conflicts.is_empty());
    }

    #[test]
    fn new_on_both_sides_merges_without_a_base() {
        let mine = edit(BASE, "LOCATION:Pool", "LOCATION:Mine\r\nX-A:1");
        let theirs = edit(BASE, "LOCATION:Pool", "LOCATION:Theirs");
        let r = run(None, Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        assert!(text.contains("LOCATION:Theirs") && text.contains("X-A:1"));
        assert_eq!(r.conflicts.len(), 1);
    }

    #[test]
    fn untouched_lines_keep_their_bytes() {
        let base = "BEGIN:VCALENDAR\nVERSION:2.0\nBEGIN:VEVENT\nUID:f\nDESCRIPTION:a long\n  folded line\nSUMMARY:x\nLOCATION:y\nEND:VEVENT\nEND:VCALENDAR\n";
        let mine = base.replace("SUMMARY:x", "SUMMARY:m");
        let theirs = base.replace("LOCATION:y", "LOCATION:t");
        let text = run(Some(base), Some(&mine), Some(&theirs)).text.unwrap();
        assert_eq!(text, base.replace("SUMMARY:x", "SUMMARY:m").replace("LOCATION:y", "LOCATION:t"));
    }

    #[test]
    fn cards_merge_their_repeatable_fields() {
        let base = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c1\r\nFN:Spike Testperson\r\nTEL;TYPE=cell:+31 6 1234 5678\r\nEMAIL:spike@example.org\r\nNOTE:old\r\nEND:VCARD\r\n";
        let mine = edit(
            &edit(base, "EMAIL:spike@example.org", "EMAIL:new@example.org"),
            "TEL;TYPE=cell:+31 6 1234 5678",
            "TEL;TYPE=cell:+31 6 1234 5678\r\nTEL;TYPE=home:+31 20 1",
        );
        let theirs = edit(
            &edit(base, "NOTE:old", "NOTE:new"),
            "TEL;TYPE=cell:+31 6 1234 5678",
            "TEL;TYPE=cell:+31 6 1234 5678\r\nitem1.TEL:+31 30 2\r\nitem1.X-ABLABEL:boat",
        );
        let r = run(Some(base), Some(&mine), Some(&theirs));
        let text = r.text.unwrap();
        for want in [
            "EMAIL:new@example.org",
            "TEL;TYPE=home:+31 20 1",
            "item1.TEL:+31 30 2",
            "item1.X-ABLABEL:boat",
            "NOTE:new",
            "TEL;TYPE=cell:+31 6 1234 5678",
        ] {
            assert!(text.contains(want), "{want} missing");
        }
        assert!(!text.contains("spike@example.org"));
        assert!(r.conflicts.is_empty());
        // The same single field on both: the server's name, the phone's reported.
        let mine = edit(base, "FN:Spike Testperson", "FN:Spike T.");
        let theirs = edit(base, "FN:Spike Testperson", "FN:Spike Test");
        let r = run(Some(base), Some(&mine), Some(&theirs));
        assert_eq!(r.text.as_deref(), Some(theirs.as_str()));
        assert_eq!(r.conflicts[0].component, "VCARD");
    }

    #[test]
    fn broken_and_mismatched_copies_are_refused() {
        assert!(matches!(merge(Some(BASE), Some("BEGIN:VEVENT\r\n"), Some("x")), Err(MergeError::Parse { .. })));
        let card = "BEGIN:VCARD\r\nFN:A\r\nEND:VCARD\r\n";
        assert_eq!(merge(Some(BASE), Some(card), Some(&edit(BASE, "Pool", "P"))), Err(MergeError::Mismatch));
        let two = format!("{card}{card}");
        assert_eq!(merge(None, Some(&two), Some(card)), Err(MergeError::NotOne(Side::Mine)));
    }

    /// Random edits on both sides of generated events: whatever only one
    /// side changed comes through, whatever neither changed stays as it
    /// was, and nothing either side added is lost.
    #[test]
    fn generated_edits_follow_the_rules() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = move |n: u64| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed % n
        };
        let singles = ["SUMMARY", "LOCATION", "DESCRIPTION", "STATUS"];
        for round in 0..3000 {
            let mut base_fields: Vec<(String, String)> = Vec::new();
            for f in singles {
                if next(3) > 0 {
                    base_fields.push((f.to_string(), format!("{f}:b{}", next(5))));
                }
            }
            let build = |fields: &[(String, String)], extra: &[String]| {
                let mut s = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:g\r\n");
                for (_, l) in fields {
                    s += l;
                    s += "\r\n";
                }
                for l in extra {
                    s += l;
                    s += "\r\n";
                }
                s + "END:VEVENT\r\nEND:VCALENDAR\r\n"
            };
            let mutate = |side: &str, n: &mut dyn FnMut(u64) -> u64| {
                let mut fields = base_fields.clone();
                let mut extra = Vec::new();
                for f in singles {
                    match n(4) {
                        0 => {
                            fields.retain(|(k, _)| k != f);
                            fields.push((f.to_string(), format!("{f}:{side}{}", n(3))));
                        }
                        1 => fields.retain(|(k, _)| k != f),
                        _ => {}
                    }
                }
                if n(2) == 0 {
                    extra.push(format!("X-{}:{}", side.to_uppercase(), n(100)));
                }
                (fields, extra)
            };
            let (m_fields, m_extra) = mutate("m", &mut next);
            let (t_fields, t_extra) = mutate("t", &mut next);
            let (b, m, t) = (build(&base_fields, &[]), build(&m_fields, &m_extra), build(&t_fields, &t_extra));
            let r = merge(Some(&b), Some(&m), Some(&t)).unwrap_or_else(|e| panic!("round {round}: {e}"));
            let text = r.text.unwrap();
            let got = lines_of(&text);
            Document::parse(&text).unwrap_or_else(|e| panic!("round {round}: {e}"));
            let value =
                |fields: &[(String, String)], f: &str| fields.iter().find(|(k, _)| k == f).map(|(_, l)| l.clone());
            for f in singles {
                let (bv, mv, tv) = (value(&base_fields, f), value(&m_fields, f), value(&t_fields, f));
                let want = if mv == bv {
                    tv.clone()
                } else if tv == bv {
                    mv.clone()
                } else {
                    tv.clone()
                };
                let have: Vec<&String> = got.iter().filter(|l| l.starts_with(&format!("{f}:"))).collect();
                assert_eq!(have.first().map(|s| s.as_str()), want.as_deref(), "round {round} field {f}");
                assert!(have.len() <= 1, "round {round}: {f} twice");
                let reported = r.conflicts.iter().any(|c| matches!(&c.lost, Lost::Field { field, .. } if field == f));
                assert_eq!(reported, mv != bv && tv != bv && mv != tv, "round {round} report {f}");
            }
            for l in m_extra.iter().chain(&t_extra) {
                assert!(got.contains(l), "round {round}: {l} lost");
            }
        }
    }
}
