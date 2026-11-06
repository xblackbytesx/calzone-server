//! What a stored object is, and the projection kept beside its text for
//! queries (decision 5).
//!
//! A calendar object follows the CalDAV resource rules (RFC 4791 section
//! 4.1): one VCALENDAR, no METHOD, one component type, one UID, at most one
//! master and no repeated RECURRENCE-ID; VTIMEZONEs and unknown components
//! are allowed. A card is one VCARD.

use chrono::{Duration, NaiveDate, NaiveDateTime};

use crate::lines::{Component, Document};
use crate::recur::{self, When};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Event,
    Todo,
    Journal,
}

impl Kind {
    pub fn component_name(self) -> &'static str {
        match self {
            Kind::Event => "VEVENT",
            Kind::Todo => "VTODO",
            Kind::Journal => "VJOURNAL",
        }
    }
}

/// The time span an object can appear in, as UTC instants. Floating times and
/// dates are widened by [`FLOATING_MARGIN`], since they land differently for
/// each viewer, so a range query never misses them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bounds {
    /// No date at all (a VTODO without DTSTART or DUE): in every range.
    Undated,
    /// `end: None` is open-ended (a rule without COUNT or UNTIL).
    Span { start: NaiveDateTime, end: Option<NaiveDateTime> },
}

pub const FLOATING_MARGIN: Duration = Duration::hours(14);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarInfo {
    pub uid: String,
    pub kind: Kind,
    /// The master's SUMMARY, unescaped (or the first component's).
    pub summary: Option<String>,
    pub bounds: Bounds,
    pub recurring: bool,
    pub warnings: Vec<recur::Warning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CardInfo {
    pub uid: Option<String>,
    pub formatted_name: Option<String>,
    pub emails: Vec<String>,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ObjectError {
    #[error("expected exactly one {0}")]
    NotOne(&'static str),
    #[error("a stored calendar object must not have METHOD")]
    Method,
    #[error("no VEVENT, VTODO or VJOURNAL")]
    NoComponent,
    #[error("more than one component type")]
    MixedTypes,
    #[error("a component has no UID")]
    MissingUid,
    #[error("components have different UIDs")]
    UidMismatch,
    #[error("more than one component without RECURRENCE-ID")]
    TwoMasters,
    #[error("the same RECURRENCE-ID twice")]
    DuplicateOverride,
}

/// Check a calendar object and compute its projection. `text` must be the
/// same object as `doc` (calcard reads the text for dates).
pub fn inspect_calendar(doc: &Document, text: &str) -> Result<CalendarInfo, ObjectError> {
    let [cal] = doc.components.as_slice() else { return Err(ObjectError::NotOne("VCALENDAR")) };
    if cal.name != "VCALENDAR" {
        return Err(ObjectError::NotOne("VCALENDAR"));
    }
    if cal.property("METHOD").is_some() {
        return Err(ObjectError::Method);
    }
    let mut kind = None;
    let mut parts: Vec<&Component> = Vec::new();
    for c in cal.components() {
        let k = match c.name.as_str() {
            "VEVENT" => Kind::Event,
            "VTODO" => Kind::Todo,
            "VJOURNAL" => Kind::Journal,
            "VFREEBUSY" => return Err(ObjectError::MixedTypes),
            _ => continue, // VTIMEZONE, X- components
        };
        if kind.is_some_and(|prev| prev != k) {
            return Err(ObjectError::MixedTypes);
        }
        kind = Some(k);
        parts.push(c);
    }
    let kind = kind.ok_or(ObjectError::NoComponent)?;
    let mut uid: Option<&str> = None;
    let mut masters = 0;
    let mut overrides: Vec<&str> = Vec::new();
    for c in &parts {
        let u = c.property("UID").map(|l| l.value()).filter(|u| !u.is_empty()).ok_or(ObjectError::MissingUid)?;
        if uid.is_some_and(|prev| prev != u) {
            return Err(ObjectError::UidMismatch);
        }
        uid = Some(u);
        match c.property("RECURRENCE-ID") {
            None => masters += 1,
            Some(r) => {
                if overrides.contains(&r.value()) {
                    return Err(ObjectError::DuplicateOverride);
                }
                overrides.push(r.value());
            }
        }
    }
    if masters > 1 {
        return Err(ObjectError::TwoMasters);
    }
    let master = parts.iter().find(|c| c.property("RECURRENCE-ID").is_none()).unwrap_or(&parts[0]);
    let summary = master.property("SUMMARY").map(|l| unescape(l.value()));
    let recurring =
        parts.iter().any(|c| c.property("RRULE").is_some() || c.property("RDATE").is_some()) || !overrides.is_empty();
    let (bounds, warnings) = bounds(&parts, text);
    Ok(CalendarInfo { uid: uid.unwrap_or_default().to_string(), kind, summary, bounds, recurring, warnings })
}

fn bounds(parts: &[&Component], text: &str) -> (Bounds, Vec<recur::Warning>) {
    let dated = parts.iter().any(|c| c.property("DTSTART").is_some() || c.property("DUE").is_some());
    if !dated {
        return (Bounds::Undated, Vec::new());
    }
    let open = parts.iter().filter_map(|c| c.property("RRULE")).any(|r| {
        let v = r.value().to_ascii_uppercase();
        !v.split(';').any(|p| p.starts_with("COUNT=") || p.starts_with("UNTIL="))
    });
    // The latest DTSTART date in the object, to bound the search for the
    // first occurrence of an open-ended rule.
    let latest_start = parts
        .iter()
        .filter_map(|c| c.property("DTSTART").or_else(|| c.property("DUE")))
        .filter_map(|l| NaiveDate::parse_from_str(l.value().get(..8)?, "%Y%m%d").ok())
        .max()
        .unwrap_or(NaiveDate::from_ymd_opt(2000, 1, 1).unwrap_or_default());
    let far_past = NaiveDate::from_ymd_opt(1800, 1, 1).unwrap_or_default().and_hms_opt(0, 0, 0).unwrap_or_default();
    let to = if open {
        latest_start.and_hms_opt(0, 0, 0).unwrap_or_default() + Duration::days(800)
    } else {
        NaiveDate::from_ymd_opt(2400, 1, 1).unwrap_or_default().and_hms_opt(0, 0, 0).unwrap_or_default()
    };
    let Ok(exp) = recur::expand(text, far_past, to, chrono_tz::UTC) else { return (Bounds::Undated, Vec::new()) };
    let widen = |w: &When, early: bool| {
        let t = w.instant(chrono_tz::UTC);
        match w {
            When::Floating(_) | When::Date(_) if early => t - FLOATING_MARGIN,
            When::Floating(_) | When::Date(_) => t + FLOATING_MARGIN,
            _ => t,
        }
    };
    let start = exp.occurrences.iter().map(|o| widen(&o.start, true)).min();
    let end = exp.occurrences.iter().map(|o| widen(&o.end, false).max(widen(&o.start, false))).max();
    let bounds = match (start, open) {
        (Some(start), true) => Bounds::Span { start, end: None },
        (Some(start), false) => Bounds::Span { start, end: end.or(Some(start)) },
        // Every instance excluded: nothing ever shows.
        (None, _) => Bounds::Span { start: far_past, end: Some(far_past) },
    };
    (bounds, exp.warnings)
}

/// Check a card and compute its projection.
pub fn inspect_card(doc: &Document) -> Result<CardInfo, ObjectError> {
    let [card] = doc.components.as_slice() else { return Err(ObjectError::NotOne("VCARD")) };
    if card.name != "VCARD" {
        return Err(ObjectError::NotOne("VCARD"));
    }
    Ok(CardInfo {
        uid: card.property("UID").map(|l| l.value().to_string()).filter(|u| !u.is_empty()),
        formatted_name: card.property("FN").map(|l| unescape(l.value())),
        emails: card.properties().filter(|l| l.name() == "EMAIL").map(|l| unescape(l.value())).collect(),
        version: card.property("VERSION").map(|l| l.value().to_string()),
    })
}

/// RFC 5545 / 6350 text unescaping: `\n` `\N` `\,` `\;` `\\`.
pub fn unescape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let mut chars = v.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n' | 'N') => out.push('\n'),
            Some(o @ (',' | ';' | '\\' | ':')) => out.push(o),
            Some(o) => {
                out.push('\\');
                out.push(o);
            }
            None => out.push('\\'),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\n{body}END:VCALENDAR\r\n")
    }

    fn info(text: &str) -> Result<CalendarInfo, ObjectError> {
        inspect_calendar(&Document::parse(text).unwrap(), text)
    }

    #[test]
    fn a_plain_event() {
        let i = info(&cal("BEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Lunch\\, with Sam\r\nDTSTART:20260601T110000Z\r\nDTEND:20260601T120000Z\r\nEND:VEVENT\r\n")).unwrap();
        assert_eq!(i.uid, "1");
        assert_eq!(i.kind, Kind::Event);
        assert_eq!(i.summary.as_deref(), Some("Lunch, with Sam"));
        assert_eq!(i.bounds, Bounds::Span { start: dt("2026-06-01 11:00"), end: Some(dt("2026-06-01 12:00")) });
        assert!(!i.recurring);
    }

    #[test]
    fn open_ended_and_bounded_rules() {
        let open = info(&cal(
            "BEGIN:VEVENT\r\nUID:2\r\nDTSTART:20260601T110000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY\r\nEND:VEVENT\r\n",
        ))
        .unwrap();
        assert_eq!(open.bounds, Bounds::Span { start: dt("2026-06-01 11:00"), end: None });
        assert!(open.recurring);
        let counted = info(&cal("BEGIN:VEVENT\r\nUID:3\r\nDTSTART:20260601T110000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nEND:VEVENT\r\n")).unwrap();
        assert_eq!(counted.bounds, Bounds::Span { start: dt("2026-06-01 11:00"), end: Some(dt("2026-06-15 12:00")) });
    }

    #[test]
    fn floating_and_all_day_are_widened() {
        let i = info(&cal(
            "BEGIN:VEVENT\r\nUID:4\r\nDTSTART;VALUE=DATE:20260601\r\nDTEND;VALUE=DATE:20260602\r\nEND:VEVENT\r\n",
        ))
        .unwrap();
        assert_eq!(i.bounds, Bounds::Span { start: dt("2026-05-31 10:00"), end: Some(dt("2026-06-02 14:00")) });
    }

    #[test]
    fn undated_todo() {
        let i = info(&cal("BEGIN:VTODO\r\nUID:5\r\nSUMMARY:Buy milk\r\nEND:VTODO\r\n")).unwrap();
        assert_eq!(i.kind, Kind::Todo);
        assert_eq!(i.bounds, Bounds::Undated);
    }

    #[test]
    fn resource_rules_are_enforced() {
        let ev = |uid: &str, rid: &str| {
            format!("BEGIN:VEVENT\r\nUID:{uid}\r\n{rid}DTSTART:20260601T110000Z\r\nEND:VEVENT\r\n")
        };
        assert_eq!(info(&cal(&format!("{}{}", ev("a", ""), ev("b", "")))), Err(ObjectError::UidMismatch));
        assert_eq!(info(&cal(&format!("{}{}", ev("a", ""), ev("a", "")))), Err(ObjectError::TwoMasters));
        let rid = "RECURRENCE-ID:20260601T110000Z\r\n";
        assert_eq!(
            info(&cal(&format!("{}{}{}", ev("a", ""), ev("a", rid), ev("a", rid)))),
            Err(ObjectError::DuplicateOverride)
        );
        assert_eq!(info(&cal("METHOD:REQUEST\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\n")), Err(ObjectError::Method));
        assert_eq!(
            info(&cal("BEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:a\r\nEND:VTODO\r\n")),
            Err(ObjectError::MixedTypes)
        );
        assert_eq!(
            info(&cal("BEGIN:VEVENT\r\nDTSTART:20260601T110000Z\r\nEND:VEVENT\r\n")),
            Err(ObjectError::MissingUid)
        );
        assert_eq!(info(&cal("")), Err(ObjectError::NoComponent));
    }

    #[test]
    fn a_card() {
        let doc = Document::parse("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c1\r\nFN:Ann\\, Smith\r\nEMAIL;TYPE=home:a@example.org\r\nitem1.EMAIL:b@example.org\r\nEND:VCARD\r\n").unwrap();
        let c = inspect_card(&doc).unwrap();
        assert_eq!(c.uid.as_deref(), Some("c1"));
        assert_eq!(c.formatted_name.as_deref(), Some("Ann, Smith"));
        assert_eq!(c.emails, vec!["a@example.org", "b@example.org"]);
        assert_eq!(c.version.as_deref(), Some("3.0"));
        let two = Document::parse("BEGIN:VCARD\r\nEND:VCARD\r\nBEGIN:VCARD\r\nEND:VCARD\r\n").unwrap();
        assert_eq!(inspect_card(&two), Err(ObjectError::NotOne("VCARD")));
    }

    #[test]
    fn unescaping() {
        assert_eq!(unescape(r"a\nb\,c\;d\\e"), "a\nb,c;d\\e");
        assert_eq!(unescape(r"keep\x"), r"keep\x");
    }
}
