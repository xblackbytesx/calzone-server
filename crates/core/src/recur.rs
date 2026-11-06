//! Occurrences of one calendar object in a time range (decision 9: the core
//! is the only place that expands recurrence).
//!
//! calcard does the RFC 5545 rule work (RRULE, RDATE, EXDATE, RECURRENCE-ID
//! overrides, THISANDFUTURE). This wrapper corrects the three behaviours
//! decision 25 lists:
//! 1. Occurrences of a UTC (`Z`) recurring event come back labelled floating.
//!    Here every occurrence is read as a wall time and interpreted with the
//!    kind of its own component's DTSTART, so UTC stays UTC.
//! 2. calcard resolves a TZID only by name. Here an unresolved TZID is
//!    evaluated from its VTIMEZONE's rules ([`crate::vtimezone`]).
//! 3. calcard's limit is one count for the whole calendar. Here each object
//!    is expanded on its own and the limit grows until the range is covered.
//! 4. calcard matches an override to its instance by SEQUENCE as well as
//!    RECURRENCE-ID, so an override with a bumped SEQUENCE (normal for
//!    Thunderbird and Google) leaves the original instance in place and the
//!    moved one is added: a duplicate. SEQUENCE never affects dates, so the
//!    copy calcard reads has no SEQUENCE lines.

use std::collections::HashMap;
use std::sync::Arc;

use calcard::common::timezone::Tz as CalTz;
use calcard::icalendar::{ICalendar, ICalendarComponent, ICalendarComponentType, ICalendarProperty, ICalendarValue};
use calcard::{Entry, Parser};
use chrono::{Duration, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone};

use crate::lines::{Document, Item};
use crate::vtimezone::VZone;

/// A point in time as the object states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum When {
    /// A UTC time (`...Z`).
    Utc(NaiveDateTime),
    /// A wall time in a named zone, with its instant.
    Zoned { local: NaiveDateTime, utc: NaiveDateTime },
    /// A wall time in whatever zone the viewer is in.
    Floating(NaiveDateTime),
    /// An all-day date.
    Date(NaiveDate),
}

impl When {
    /// The instant this is for a viewer in `viewer`: floating times and dates
    /// are read in the viewer's zone.
    pub fn instant(&self, viewer: chrono_tz::Tz) -> NaiveDateTime {
        match *self {
            When::Utc(t) | When::Zoned { utc: t, .. } => t,
            When::Floating(l) => iana_to_utc(viewer, l),
            When::Date(d) => iana_to_utc(viewer, d.and_hms_opt(0, 0, 0).unwrap_or_default()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Occurrence {
    pub start: When,
    pub end: When,
    /// The instance's RECURRENCE-ID: its original start. None for an event
    /// that does not recur.
    pub recurrence_id: Option<When>,
    /// Whether an override component (RECURRENCE-ID) supplied this instance.
    pub overridden: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Warning {
    /// A TZID neither a name lookup nor its VTIMEZONE could resolve; its times
    /// are treated as floating.
    UnknownTimezone(String),
    /// A VTIMEZONE whose rules [`VZone`] cannot evaluate; its times are
    /// treated as floating.
    UnsupportedTimezone(String),
    /// calcard reported a component it could not date (no or bad DTSTART).
    Undated,
    /// More occurrences than [`MAX_OCCURRENCES`] before the range ends.
    Truncated,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Expansion {
    /// Sorted by start instant (for the viewer).
    pub occurrences: Vec<Occurrence>,
    pub warnings: Vec<Warning>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExpandError {
    #[error("not an iCalendar object")]
    NotCalendar,
}

/// The cap on occurrences expanded for one object, whatever the range.
pub const MAX_OCCURRENCES: usize = 200_000;

/// Occurrences overlapping `[from, to)` (UTC), floating times read in
/// `viewer`. A zero-length occurrence counts when it starts in the range.
pub fn expand(
    text: &str,
    from: NaiveDateTime,
    to: NaiveDateTime,
    viewer: chrono_tz::Tz,
) -> Result<Expansion, ExpandError> {
    let doc = Document::parse(text).ok();
    let for_calcard = doc.as_ref().map(without_sequence);
    let Entry::ICalendar(cal) = Parser::new(for_calcard.as_deref().unwrap_or(text)).entry() else {
        return Err(ExpandError::NotCalendar);
    };
    let mut zones = Zones::new(&cal, doc.as_ref());
    let mut out = Expansion::default();

    let mut limit = 1024;
    let raw = loop {
        let exp = cal.expand_dates(CalTz::Floating, limit);
        let latest = exp.events.iter().map(|e| e.start.naive_local()).max();
        // Done when the rule ran out before the limit, or reached the range end.
        // Wall times are compared with a day of margin for any zone offset.
        let covered = latest.is_some_and(|l| l > to + Duration::days(1));
        if exp.events.len() < limit.saturating_sub(64) || covered {
            break exp;
        }
        if limit >= MAX_OCCURRENCES {
            out.warnings.push(Warning::Truncated);
            break exp;
        }
        limit = (limit * 4).min(MAX_OCCURRENCES);
    };
    if !raw.errors.is_empty() {
        out.warnings.push(Warning::Undated);
    }

    for e in &raw.events {
        let comp = &cal.components[e.comp_id as usize];
        let start_kind = zones.kind_of(comp, Field::Start, &mut out.warnings);
        let start_wall = e.start.naive_local();
        let start = start_kind.interpret(start_wall);
        let end = match &e.end {
            calcard::icalendar::dates::TimeOrDelta::Time(t) => {
                zones.kind_of(comp, Field::End, &mut out.warnings).or(&start_kind).interpret(t.naive_local())
            }
            calcard::icalendar::dates::TimeOrDelta::Delta(d) => start_kind.interpret(start_wall + *d),
        };
        let overridden = has(comp, &ICalendarProperty::RecurrenceId);
        let recurring = overridden || has(comp, &ICalendarProperty::Rrule) || has(comp, &ICalendarProperty::Rdate);
        let recurrence_id = if overridden {
            recurrence_id_of(comp, &mut zones, &mut out.warnings)
        } else if recurring {
            Some(start)
        } else {
            None
        };
        let (s, mut en) = (start.instant(viewer), end.instant(viewer));
        if en < s {
            en = s;
        }
        let hit = s < to && (en > from || (en == s && s >= from));
        if hit {
            out.occurrences.push(Occurrence { start, end, recurrence_id, overridden });
        }
    }
    out.occurrences.sort_by_key(|o| o.start.instant(viewer));
    out.warnings.dedup();
    Ok(out)
}

/// The object without SEQUENCE in its scheduling components (fix 4). Only
/// properties go, never components, so calcard's component ids still line up.
fn without_sequence(doc: &Document) -> String {
    let mut doc = doc.clone();
    for cal in doc.components.iter_mut() {
        for item in cal.items.iter_mut() {
            if let Item::Component(c) = item
                && matches!(c.name.as_str(), "VEVENT" | "VTODO" | "VJOURNAL")
            {
                c.items.retain(|i| !matches!(i, Item::Property(l) if l.name() == "SEQUENCE"));
            }
        }
    }
    doc.to_string()
}

fn has(comp: &ICalendarComponent, name: &ICalendarProperty) -> bool {
    comp.entries.iter().any(|e| &e.name == name)
}

fn recurrence_id_of(comp: &ICalendarComponent, zones: &mut Zones<'_>, warnings: &mut Vec<Warning>) -> Option<When> {
    let entry = comp.entries.iter().find(|e| e.name == ICalendarProperty::RecurrenceId)?;
    let kind = zones.kind_of_entry(entry, warnings);
    let wall = wall_time(entry)?;
    Some(kind.interpret(wall))
}

fn wall_time(entry: &calcard::icalendar::ICalendarEntry) -> Option<NaiveDateTime> {
    let Some(ICalendarValue::PartialDateTime(p)) = entry.values.first() else { return None };
    let date = NaiveDate::from_ymd_opt(p.year? as i32, p.month? as u32, p.day? as u32)?;
    date.and_hms_opt(p.hour.unwrap_or(0) as u32, p.minute.unwrap_or(0) as u32, p.second.unwrap_or(0) as u32)
}

#[derive(Clone, Copy)]
enum Field {
    Start,
    End,
}

/// How a component's times are to be read.
#[derive(Debug, Clone)]
enum Kind {
    Utc,
    Date,
    Floating,
    Iana(chrono_tz::Tz),
    Fixed(i32),
    Rules(Arc<VZone>),
    /// No DTEND/DUE of its own: use the start's kind.
    Unset,
}

impl Kind {
    fn or<'a>(&'a self, other: &'a Kind) -> &'a Kind {
        if matches!(self, Kind::Unset) { other } else { self }
    }

    fn interpret(&self, wall: NaiveDateTime) -> When {
        match self {
            Kind::Utc => When::Utc(wall),
            Kind::Date => When::Date(wall.date()),
            Kind::Floating | Kind::Unset => When::Floating(wall),
            Kind::Iana(tz) => When::Zoned { local: wall, utc: iana_to_utc(*tz, wall) },
            Kind::Fixed(off) => When::Zoned { local: wall, utc: wall - Duration::seconds(*off as i64) },
            Kind::Rules(z) => When::Zoned { local: wall, utc: z.local_to_utc(wall) },
        }
    }
}

/// A wall time in an IANA zone to UTC: gaps use the offset before the gap,
/// overlaps the earlier instant (RFC 5545 3.3.5).
fn iana_to_utc(tz: chrono_tz::Tz, wall: NaiveDateTime) -> NaiveDateTime {
    match tz.from_local_datetime(&wall) {
        LocalResult::Single(t) => t.naive_utc(),
        LocalResult::Ambiguous(a, b) => a.naive_utc().min(b.naive_utc()),
        LocalResult::None => {
            let before = tz.offset_from_utc_datetime(&(wall - Duration::days(1))).fix().local_minus_utc();
            wall - Duration::seconds(before as i64)
        }
    }
}

/// TZID resolution, cached per object: a name lookup (calcard's, which also
/// knows `/vendor/.../Region/City` prefixes and Windows names), then the
/// VTIMEZONE's own location hints, then the VTIMEZONE's rules.
struct Zones<'a> {
    cal: &'a ICalendar,
    doc: Option<&'a Document>,
    cache: HashMap<String, Kind>,
}

impl<'a> Zones<'a> {
    fn new(cal: &'a ICalendar, doc: Option<&'a Document>) -> Self {
        Self { cal, doc, cache: HashMap::new() }
    }

    fn kind_of(&mut self, comp: &ICalendarComponent, field: Field, warnings: &mut Vec<Warning>) -> Kind {
        let names: &[ICalendarProperty] = match field {
            Field::Start => &[ICalendarProperty::Dtstart, ICalendarProperty::Due],
            Field::End => &[ICalendarProperty::Dtend, ICalendarProperty::Due],
        };
        let entry = names.iter().find_map(|n| comp.entries.iter().find(|e| &e.name == n));
        match entry {
            Some(e) => self.kind_of_entry(e, warnings),
            None => Kind::Unset,
        }
    }

    fn kind_of_entry(&mut self, entry: &calcard::icalendar::ICalendarEntry, warnings: &mut Vec<Warning>) -> Kind {
        let Some(ICalendarValue::PartialDateTime(p)) = entry.values.first() else { return Kind::Floating };
        if p.hour.is_none() {
            return Kind::Date;
        }
        if p.tz_hour.is_some() {
            let off = (p.tz_hour.unwrap_or(0) as i32 * 3600 + p.tz_minute.unwrap_or(0) as i32 * 60)
                * if p.tz_minus { -1 } else { 1 };
            return if off == 0 { Kind::Utc } else { Kind::Fixed(off) };
        }
        match entry.tz_id() {
            Some(tzid) => self.resolve(tzid, warnings),
            None => Kind::Floating,
        }
    }

    fn resolve(&mut self, tzid: &str, warnings: &mut Vec<Warning>) -> Kind {
        if let Some(k) = self.cache.get(tzid) {
            return k.clone();
        }
        let from_cal = |tz: CalTz| match tz {
            CalTz::Tz(t) => Some(Kind::Iana(t)),
            CalTz::Fixed(o) => Some(Kind::Fixed(o.local_minus_utc())),
            CalTz::Floating => None,
        };
        let by_name = tzid.parse::<CalTz>().ok().and_then(from_cal);
        let by_hints = || {
            self.cal
                .components
                .iter()
                .filter(|c| c.component_type == ICalendarComponentType::VTimezone)
                .filter_map(|c| c.timezone())
                .find(|(name, _)| *name == tzid)
                .and_then(|(_, tz)| from_cal(tz))
        };
        let kind = match by_name.or_else(by_hints) {
            Some(k) => k,
            None => match self.vtimezone(tzid) {
                Some(Ok(z)) => Kind::Rules(Arc::new(z)),
                Some(Err(_)) => {
                    warnings.push(Warning::UnsupportedTimezone(tzid.to_string()));
                    Kind::Floating
                }
                None => {
                    warnings.push(Warning::UnknownTimezone(tzid.to_string()));
                    Kind::Floating
                }
            },
        };
        self.cache.insert(tzid.to_string(), kind.clone());
        kind
    }

    fn vtimezone(&self, tzid: &str) -> Option<Result<VZone, crate::vtimezone::VZoneError>> {
        let cal = self.doc?.components.iter().find(|c| c.name == "VCALENDAR")?;
        let vtz = cal
            .components()
            .find(|c| c.name == "VTIMEZONE" && c.property("TZID").is_some_and(|l| l.value() == tzid))?;
        Some(VZone::from_component(vtz))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn starts(text: &str, viewer: chrono_tz::Tz) -> Vec<When> {
        expand(text, dt("2000-01-01 00:00"), dt("2100-01-01 00:00"), viewer)
            .unwrap()
            .occurrences
            .iter()
            .map(|o| o.start)
            .collect()
    }

    const AMS: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Amsterdam\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nDTSTART:19810329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nDTSTART:19961027T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\n{body}END:VCALENDAR\r\n")
    }

    #[test]
    fn zoned_weekly_event_keeps_its_wall_time_across_dst() {
        let text = cal(&format!(
            "{AMS}BEGIN:VEVENT\r\nUID:a\r\nDTSTART;TZID=Europe/Amsterdam:20261019T090000\r\nDTEND;TZID=Europe/Amsterdam:20261019T100000\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nEND:VEVENT\r\n"
        ));
        let got = starts(&text, chrono_tz::UTC);
        let utc: Vec<NaiveDateTime> = got.iter().map(|w| w.instant(chrono_tz::UTC)).collect();
        assert_eq!(utc, vec![dt("2026-10-19 07:00"), dt("2026-10-26 08:00"), dt("2026-11-02 08:00")]);
        assert!(matches!(got[1], When::Zoned { local, .. } if local == dt("2026-10-26 09:00")));
    }

    #[test]
    fn utc_recurring_event_stays_utc() {
        // calcard labels these floating; read in Amsterdam they would shift by an hour.
        let text = cal(
            "BEGIN:VEVENT\r\nUID:b\r\nDTSTART:20260105T120000Z\r\nDTEND:20260105T130000Z\r\nRRULE:FREQ=DAILY;COUNT=2\r\nEND:VEVENT\r\n",
        );
        let got = starts(&text, chrono_tz::Europe::Amsterdam);
        assert_eq!(got, vec![When::Utc(dt("2026-01-05 12:00")), When::Utc(dt("2026-01-06 12:00"))]);
    }

    #[test]
    fn custom_tzid_is_evaluated_from_its_vtimezone() {
        let zone = AMS.replace("TZID:Europe/Amsterdam", "TZID:My Custom Zone");
        let text = cal(&format!(
            "{zone}BEGIN:VEVENT\r\nUID:c\r\nDTSTART;TZID=My Custom Zone:20261019T090000\r\nDTEND;TZID=My Custom Zone:20261019T100000\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nEND:VEVENT\r\n"
        ));
        let e = expand(&text, dt("2000-01-01 00:00"), dt("2100-01-01 00:00"), chrono_tz::UTC).unwrap();
        assert!(e.warnings.is_empty(), "{:?}", e.warnings);
        let utc: Vec<NaiveDateTime> = e.occurrences.iter().map(|o| o.start.instant(chrono_tz::UTC)).collect();
        assert_eq!(utc, vec![dt("2026-10-19 07:00"), dt("2026-10-26 08:00"), dt("2026-11-02 08:00")]);
        let ends: Vec<NaiveDateTime> = e.occurrences.iter().map(|o| o.end.instant(chrono_tz::UTC)).collect();
        assert_eq!(ends[1], dt("2026-10-26 09:00"));
    }

    #[test]
    fn unknown_tzid_without_vtimezone_warns_and_floats() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:d\r\nDTSTART;TZID=Nowhere/Special:20260101T090000\r\nDTEND;TZID=Nowhere/Special:20260101T100000\r\nEND:VEVENT\r\n",
        );
        let e = expand(&text, dt("2000-01-01 00:00"), dt("2100-01-01 00:00"), chrono_tz::UTC).unwrap();
        assert_eq!(e.warnings, vec![Warning::UnknownTimezone("Nowhere/Special".into())]);
        assert_eq!(e.occurrences[0].start, When::Floating(dt("2026-01-01 09:00")));
    }

    #[test]
    fn exdate_rdate_and_override() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20260105T120000Z\r\nDTEND:20260105T130000Z\r\nRRULE:FREQ=DAILY;COUNT=4\r\nEXDATE:20260106T120000Z\r\nRDATE:20260110T120000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:e\r\nRECURRENCE-ID:20260107T120000Z\r\nDTSTART:20260107T150000Z\r\nDTEND:20260107T160000Z\r\nEND:VEVENT\r\n",
        );
        let e = expand(&text, dt("2000-01-01 00:00"), dt("2100-01-01 00:00"), chrono_tz::UTC).unwrap();
        let got: Vec<(When, Option<When>, bool)> =
            e.occurrences.iter().map(|o| (o.start, o.recurrence_id, o.overridden)).collect();
        assert_eq!(
            got,
            vec![
                (When::Utc(dt("2026-01-05 12:00")), Some(When::Utc(dt("2026-01-05 12:00"))), false),
                (When::Utc(dt("2026-01-07 15:00")), Some(When::Utc(dt("2026-01-07 12:00"))), true),
                (When::Utc(dt("2026-01-08 12:00")), Some(When::Utc(dt("2026-01-08 12:00"))), false),
                (When::Utc(dt("2026-01-10 12:00")), Some(When::Utc(dt("2026-01-10 12:00"))), false),
            ]
        );
    }

    #[test]
    fn an_override_with_a_bumped_sequence_replaces_its_instance() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:s\r\nDTSTART:20260105T120000Z\r\nDTEND:20260105T130000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:s\r\nSEQUENCE:2\r\nRECURRENCE-ID:20260106T120000Z\r\nDTSTART:20260106T150000Z\r\nDTEND:20260106T160000Z\r\nEND:VEVENT\r\n",
        );
        let got = starts(&text, chrono_tz::UTC);
        assert_eq!(
            got,
            vec![
                When::Utc(dt("2026-01-05 12:00")),
                When::Utc(dt("2026-01-06 15:00")),
                When::Utc(dt("2026-01-07 12:00"))
            ]
        );
    }

    #[test]
    fn this_and_future_moves_the_rest_of_the_series() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:f\r\nDTSTART:20260105T120000Z\r\nDURATION:PT1H\r\nRRULE:FREQ=DAILY;COUNT=4\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:f\r\nRECURRENCE-ID;RANGE=THISANDFUTURE:20260107T120000Z\r\nDTSTART:20260107T140000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n",
        );
        let got: Vec<NaiveDateTime> = starts(&text, chrono_tz::UTC).iter().map(|w| w.instant(chrono_tz::UTC)).collect();
        assert_eq!(
            got,
            vec![dt("2026-01-05 12:00"), dt("2026-01-06 12:00"), dt("2026-01-07 14:00"), dt("2026-01-08 14:00")]
        );
    }

    #[test]
    fn last_weekday_of_the_month() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:g\r\nDTSTART:20260130T090000Z\r\nDTEND:20260130T100000Z\r\nRRULE:FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1;COUNT=3\r\nEND:VEVENT\r\n",
        );
        let got = starts(&text, chrono_tz::UTC);
        assert_eq!(
            got,
            vec![
                When::Utc(dt("2026-01-30 09:00")),
                When::Utc(dt("2026-02-27 09:00")),
                When::Utc(dt("2026-03-31 09:00"))
            ]
        );
    }

    #[test]
    fn all_day_and_feb_29() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:h\r\nDTSTART;VALUE=DATE:20240229\r\nDTEND;VALUE=DATE:20240301\r\nRRULE:FREQ=YEARLY;COUNT=2\r\nEND:VEVENT\r\n",
        );
        let e = expand(&text, dt("2000-01-01 00:00"), dt("2100-01-01 00:00"), chrono_tz::UTC).unwrap();
        let d = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
        assert_eq!(
            e.occurrences.iter().map(|o| o.start).collect::<Vec<_>>(),
            vec![When::Date(d("2024-02-29")), When::Date(d("2028-02-29"))]
        );
        assert_eq!(e.occurrences[0].end, When::Date(d("2024-03-01")));
    }

    #[test]
    fn open_ended_rules_stop_at_the_range() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:i\r\nDTSTART:19900101T080000Z\r\nDTEND:19900101T083000Z\r\nRRULE:FREQ=DAILY\r\nEND:VEVENT\r\n",
        );
        let e = expand(&text, dt("2030-03-01 00:00"), dt("2030-03-08 00:00"), chrono_tz::UTC).unwrap();
        assert_eq!(e.occurrences.len(), 7);
        assert!(e.warnings.is_empty());
        let e = expand(&text, dt("2030-03-01 00:00"), dt("2700-01-01 00:00"), chrono_tz::UTC).unwrap();
        assert_eq!(e.warnings, vec![Warning::Truncated]);
    }

    #[test]
    fn floating_times_follow_the_viewer() {
        let text = cal("BEGIN:VEVENT\r\nUID:j\r\nDTSTART:20260601T090000\r\nDTEND:20260601T100000\r\nEND:VEVENT\r\n");
        let e = expand(&text, dt("2026-06-01 06:00"), dt("2026-06-01 08:00"), chrono_tz::Europe::Amsterdam).unwrap();
        assert_eq!(e.occurrences.len(), 1, "09:00 in Amsterdam is 07:00Z");
        let e = expand(&text, dt("2026-06-01 06:00"), dt("2026-06-01 08:00"), chrono_tz::UTC).unwrap();
        assert!(e.occurrences.is_empty());
    }

    #[test]
    fn range_overlap_rules() {
        let text = cal("BEGIN:VEVENT\r\nUID:k\r\nDTSTART:20260601T090000Z\r\nDTEND:20260601T100000Z\r\nEND:VEVENT\r\n");
        let n = |a, b| expand(&text, dt(a), dt(b), chrono_tz::UTC).unwrap().occurrences.len();
        assert_eq!(n("2026-06-01 09:30", "2026-06-01 09:45"), 1);
        assert_eq!(n("2026-06-01 10:00", "2026-06-01 11:00"), 0, "ends exactly at the range start");
        assert_eq!(n("2026-06-01 08:00", "2026-06-01 09:00"), 0, "starts exactly at the range end");
    }
}
