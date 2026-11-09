//! Android's calendar (decision 49): what the system bridge writes into
//! `CalendarContract.Events` for a stored event, and how an edit another app
//! made there becomes an edit of the stored text.
//!
//! - [`rows`]: the projection. One row for the event (the series), one per
//!   moved or canceled occurrence (an exception row), with Android's own
//!   conventions, from DAVx5's synctools (GPL-3.0): times as UTC
//!   milliseconds with an IANA zone id; all-day events at UTC midnight in
//!   zone `UTC`; floating times in the phone's zone; DTEND for a single
//!   event and DURATION (`P<n>S` or `P<n>D`) for a series; RDATE and EXDATE
//!   as `yyyymmddThhmmssZ` lists (all-day ones at `T000000Z`), the series'
//!   start first among RDATEs, and RDATEs dropped next to an endless RRULE
//!   (the provider cannot expand both); exceptions keyed by their original
//!   instance time. Reminders are the person's own (decision 8): display
//!   alarms relative to the start, in minutes before it.
//! - [`apply`]: an edited row back to the stored text. It is compared with
//!   what the projection wrote, field by field and by meaning (Fossify
//!   rewrites `RRULE` and `DURATION` in equal forms, decision 45), and only
//!   the fields that changed go through [`crate::edit`], so everything else
//!   in the object stays byte for byte (decision 5). A canceled exception
//!   row deletes that occurrence (an EXDATE).
//! - [`new_event`]: a row made in another app, as a new object.
//!
//! Android expands recurrence itself for the apps that read its tables;
//! CalZone's own screens expand with the core (decision 9, exception in
//! decision 49). Pure: the phone's zone and the clock are passed in.

use chrono::{Duration, NaiveDate, NaiveDateTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::edit::{self, EditError, EventChange, Scope};
use crate::lines::{Component, Document, Line};
use crate::moment::{self, Key, Moment};
use crate::object::unescape;
use crate::recur::{self, When};

/// `CalendarContract.Events.STATUS_*`.
pub const STATUS_TENTATIVE: i32 = 0;
pub const STATUS_CONFIRMED: i32 = 1;
pub const STATUS_CANCELED: i32 = 2;

/// One `Events` row, as the bridge writes it and as it reads an edited one
/// back. Times are UTC milliseconds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct EventRow {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub dtstart: i64,
    /// Single events and exceptions.
    #[serde(default)]
    pub dtend: Option<i64>,
    /// A series (`P3600S`, `P1D`).
    #[serde(default)]
    pub duration: Option<String>,
    pub all_day: bool,
    /// An IANA zone id, or `UTC`.
    pub timezone: String,
    #[serde(default)]
    pub end_timezone: Option<String>,
    #[serde(default)]
    pub rrule: Option<String>,
    #[serde(default)]
    pub rdate: Option<String>,
    #[serde(default)]
    pub exrule: Option<String>,
    #[serde(default)]
    pub exdate: Option<String>,
    #[serde(default)]
    pub status: Option<i32>,
    /// 0 busy, 1 free.
    #[serde(default)]
    pub availability: i32,
    /// 0 default, 1 confidential, 2 private, 3 public.
    #[serde(default)]
    pub access_level: i32,
    #[serde(default)]
    pub uid: Option<String>,
    /// Exceptions: the occurrence's original start.
    #[serde(default)]
    pub original_instance: Option<i64>,
    #[serde(default)]
    pub original_all_day: Option<bool>,
    /// Minutes before the start.
    #[serde(default)]
    pub reminders: Vec<i64>,
}

/// The rows for one stored event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventRows {
    pub main: EventRow,
    pub exceptions: Vec<EventRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BridgeError {
    #[error("not an event Android can show")]
    NotAnEvent,
    #[error("no such occurrence")]
    NoSuchInstance,
    #[error(transparent)]
    Edit(#[from] EditError),
}

// ---------------------------------------------------------------------------
// Times.

fn ms(t: NaiveDateTime) -> i64 {
    t.and_utc().timestamp_millis()
}

fn from_ms(v: i64) -> NaiveDateTime {
    chrono::DateTime::from_timestamp_millis(v).map(|d| d.naive_utc()).unwrap_or_default()
}

fn iana(tzid: &str) -> Option<chrono_tz::Tz> {
    tzid.parse().ok()
}

fn is_utc_id(tz: &str) -> bool {
    matches!(tz, "UTC" | "Etc/UTC" | "GMT" | "Etc/GMT" | "Z")
}

fn local_to_utc(tz: chrono_tz::Tz, local: NaiveDateTime) -> NaiveDateTime {
    tz.from_local_datetime(&local)
        .earliest()
        .or_else(|| tz.from_local_datetime(&(local + Duration::hours(1))).earliest())
        .map(|t| t.naive_utc())
        .unwrap_or(local)
}

fn utc_to_local(tz: chrono_tz::Tz, utc: NaiveDateTime) -> NaiveDateTime {
    tz.from_utc_datetime(&utc).naive_local()
}

/// A moment as Android stores it: UTC milliseconds, the zone id, all-day.
fn android_time(m: &Moment, doc: &Document, phone: chrono_tz::Tz) -> (i64, String, bool) {
    match m {
        Moment::Date(d) => (ms(d.and_hms_opt(0, 0, 0).expect("midnight")), "UTC".into(), true),
        Moment::Utc(t) => (ms(*t), "UTC".into(), false),
        Moment::Floating(t) => (ms(local_to_utc(phone, *t)), phone.name().into(), false),
        Moment::Zoned { local, tzid } => match iana(tzid) {
            Some(tz) => (ms(local_to_utc(tz, *local)), tz.name().into(), false),
            // A zone only the object's VTIMEZONE knows: Android gets UTC.
            None => (ms(moment::zoned_to_utc(*local, tzid, doc)), "UTC".into(), false),
        },
    }
}

/// An Android time as a moment of the same kind as `like` where the zone
/// did not change, so a floating or zoned event stays what it was.
fn moment_from(v: i64, tz: &str, all_day: bool, like: Option<&Moment>, phone: chrono_tz::Tz) -> Moment {
    let utc = from_ms(v);
    if all_day {
        return Moment::Date(utc.date());
    }
    match like {
        Some(Moment::Floating(_)) if tz == phone.name() => return Moment::Floating(utc_to_local(phone, utc)),
        Some(Moment::Zoned { tzid, .. }) if iana(tzid).is_some_and(|z| z.name() == tz) => {
            let z = iana(tzid).expect("checked");
            return Moment::Zoned { local: utc_to_local(z, utc), tzid: tzid.clone() };
        }
        _ => {}
    }
    if is_utc_id(tz) {
        return Moment::Utc(utc);
    }
    match iana(tz) {
        Some(z) => Moment::Zoned { local: utc_to_local(z, utc), tzid: z.name().to_string() },
        None => Moment::Utc(utc),
    }
}

/// Android's DURATION: `P3600S`, `PT1H`, `P1D`, `P0DT1H0M0S`, `P1W`. `M` is
/// always minutes, as Android reads it.
pub fn parse_android_duration(v: &str) -> Option<Duration> {
    let v = v.trim();
    let (neg, v) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut any = false;
    for ch in v.chars() {
        match ch {
            'T' => {}
            '0'..='9' => num.push(ch),
            u => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                any = true;
                total += match u {
                    'W' => Duration::weeks(n),
                    'D' => Duration::days(n),
                    'H' => Duration::hours(n),
                    'M' => Duration::minutes(n),
                    'S' => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    (any && num.is_empty()).then_some(if neg { -total } else { total })
}

/// The row's end in UTC milliseconds: DTEND, else the start plus DURATION,
/// else the start (a day later for all-day).
fn end_ms(r: &EventRow) -> i64 {
    if let Some(e) = r.dtend {
        return e;
    }
    if let Some(d) = r.duration.as_deref().and_then(parse_android_duration) {
        return r.dtstart + d.num_milliseconds();
    }
    if r.all_day { r.dtstart + 86_400_000 } else { r.dtstart }
}

// ---------------------------------------------------------------------------
// Reading the stored event.

fn text_of(c: &Component, name: &str) -> Option<String> {
    c.property(name).map(|l| unescape(l.value())).filter(|s| !s.is_empty())
}

fn moment_of(c: &Component, name: &str) -> Option<Moment> {
    c.property(name).and_then(Moment::from_line)
}

/// The stated end: DTEND, else DTSTART + DURATION, else the RFC default.
fn end_of(c: &Component, start: &Moment) -> Moment {
    if let Some(e) = moment_of(c, "DTEND") {
        return e;
    }
    if let Some(d) = c.property("DURATION").and_then(|l| edit::parse_duration(l.value())) {
        return start.shifted(d);
    }
    if start.is_date() { start.shifted(Duration::days(1)) } else { start.clone() }
}

fn reminders(c: &Component) -> Vec<i64> {
    let mut out: Vec<i64> = c
        .components()
        .filter(|a| a.name == "VALARM")
        .filter(|a| a.property("ACTION").is_none_or(|l| !l.value().eq_ignore_ascii_case("EMAIL")))
        .filter_map(|a| a.property("TRIGGER"))
        .filter(|t| t.param("VALUE").is_none_or(|v| !v.eq_ignore_ascii_case("DATE-TIME")))
        .filter(|t| t.param("RELATED").is_none_or(|v| !v.eq_ignore_ascii_case("END")))
        .filter_map(|t| edit::parse_duration(t.value()))
        .map(|d| -d.num_minutes())
        .filter(|m| *m >= 0)
        .collect();
    out.sort_unstable();
    out
}

fn status(c: &Component) -> Option<i32> {
    c.property("STATUS").map(|l| match l.value().to_ascii_uppercase().as_str() {
        "CONFIRMED" => STATUS_CONFIRMED,
        "CANCELLED" => STATUS_CANCELED,
        _ => STATUS_TENTATIVE,
    })
}

/// Each value of a date-list property (RDATE, EXDATE) as a moment, with the
/// property's own parameters.
fn list_moments(line: &Line) -> Vec<Moment> {
    let src = line.text().to_string();
    // The head is everything before the first unquoted colon.
    let mut quoted = false;
    let mut at = None;
    for (i, ch) in src.char_indices() {
        match ch {
            '"' => quoted = !quoted,
            ':' if !quoted => {
                at = Some(i);
                break;
            }
            _ => {}
        }
    }
    let Some(at) = at else { return Vec::new() };
    let head = &src[..at];
    let params = head.find(';').map(|i| &head[i..]).unwrap_or("");
    line.value()
        .split(',')
        .filter(|v| !v.trim().is_empty() && !v.contains('/'))
        .filter_map(|v| Moment::from_line(&Line::new(format!("X{params}:{}", v.trim()))))
        .collect()
}

/// A date-list value at the series' kind of start: a date for all-day, the
/// start's clock time on that date for a date in a timed series.
fn aligned(m: &Moment, start: &Moment) -> Moment {
    match (start, m) {
        (Moment::Date(_), Moment::Date(_)) => m.clone(),
        (Moment::Date(_), other) => Moment::Date(match other.key() {
            Key::Date(d) => d,
            Key::Time(t) => t.date(),
        }),
        (_, Moment::Date(d)) => match start.key() {
            Key::Time(t) => start.with_key(Key::Time(d.and_time(t.time()))),
            Key::Date(_) => m.clone(),
        },
        _ => m.clone(),
    }
}

fn date_list(moments: &[Moment], start: &Moment, doc: &Document, phone: chrono_tz::Tz) -> String {
    moments
        .iter()
        .map(|m| {
            let (v, _, _) = android_time(&aligned(m, start), doc, phone);
            format!("{}Z", from_ms(v).format("%Y%m%dT%H%M%S"))
        })
        .collect::<Vec<_>>()
        .join(",")
}

fn is_endless(rule: &str) -> bool {
    !rule.split(';').any(|p| {
        let k = p.split('=').next().unwrap_or("").trim().to_ascii_uppercase();
        k == "COUNT" || k == "UNTIL"
    })
}

struct Event<'a> {
    master: &'a Component,
    overrides: Vec<&'a Component>,
}

fn event(doc: &Document) -> Option<Event<'_>> {
    let cal = doc.components.first().filter(|c| c.name == "VCALENDAR")?;
    let events: Vec<&Component> = cal.components().filter(|c| c.name == "VEVENT").collect();
    let master = events.iter().copied().find(|c| c.property("RECURRENCE-ID").is_none())?;
    let overrides = events.into_iter().filter(|c| c.property("RECURRENCE-ID").is_some()).collect();
    Some(Event { master, overrides })
}

fn base_row(c: &Component, start: &Moment, doc: &Document, phone: chrono_tz::Tz) -> EventRow {
    let (dtstart, timezone, all_day) = android_time(start, doc, phone);
    EventRow {
        title: text_of(c, "SUMMARY"),
        location: text_of(c, "LOCATION"),
        description: text_of(c, "DESCRIPTION"),
        dtstart,
        all_day,
        timezone,
        status: status(c),
        availability: i32::from(c.property("TRANSP").is_some_and(|l| l.value().eq_ignore_ascii_case("TRANSPARENT"))),
        access_level: match c.property("CLASS").map(|l| l.value().to_ascii_uppercase()) {
            Some(v) if v == "CONFIDENTIAL" => 1,
            Some(v) if v == "PRIVATE" => 2,
            Some(v) if v == "PUBLIC" => 3,
            _ => 0,
        },
        uid: c.property("UID").map(|l| l.value().to_string()),
        reminders: reminders(c),
        ..EventRow::default()
    }
}

/// A single event's (or an exception's) end: after the start, else the
/// default (synctools: Android needs DTEND after DTSTART, a day for all-day).
fn single_end(row: &mut EventRow, end: &Moment, doc: &Document, phone: chrono_tz::Tz) {
    let (e, etz, _) = android_time(end, doc, phone);
    let e = if e > row.dtstart || (!row.all_day && e == row.dtstart) {
        e
    } else if row.all_day {
        row.dtstart + 86_400_000
    } else {
        row.dtstart
    };
    row.dtend = Some(e);
    row.end_timezone = Some(etz);
}

/// The rows for a stored event, or `None` for what Android's calendar does
/// not hold (tasks, journals, an object with only moved occurrences).
pub fn rows(text: &str, phone: chrono_tz::Tz) -> Option<EventRows> {
    let doc = Document::parse(text).ok()?;
    let ev = event(&doc)?;
    let m = ev.master;
    let start = moment_of(m, "DTSTART")?;
    let end = end_of(m, &start);
    let mut main = base_row(m, &start, &doc, phone);
    let rrules: Vec<String> = m.properties().filter(|l| l.name() == "RRULE").map(|l| l.value().to_string()).collect();
    let rdates: Vec<Moment> = m.properties().filter(|l| l.name() == "RDATE").flat_map(list_moments).collect();
    let recurring = !rrules.is_empty() || !rdates.is_empty();
    if recurring {
        let (e, _, _) = android_time(&end, &doc, phone);
        let len = (e - main.dtstart).max(0);
        main.duration =
            Some(if main.all_day { format!("P{}D", (len / 86_400_000).max(1)) } else { format!("P{}S", len / 1000) });
        if !rrules.is_empty() {
            main.rrule = Some(rrules.join("\n"));
        }
        let endless = rrules.iter().any(|r| is_endless(r));
        if !rdates.is_empty() && !endless {
            let mut with_start = vec![start.clone()];
            with_start.extend(rdates);
            main.rdate = Some(date_list(&with_start, &start, &doc, phone));
        }
        let exrules: Vec<String> =
            m.properties().filter(|l| l.name() == "EXRULE").map(|l| l.value().to_string()).collect();
        if !exrules.is_empty() {
            main.exrule = Some(exrules.join("\n"));
        }
        let exdates: Vec<Moment> = m.properties().filter(|l| l.name() == "EXDATE").flat_map(list_moments).collect();
        if !exdates.is_empty() {
            main.exdate = Some(date_list(&exdates, &start, &doc, phone));
        }
    } else {
        single_end(&mut main, &end, &doc, phone);
    }
    let exceptions = if recurring {
        ev.overrides
            .iter()
            .filter_map(|o| {
                let rid = moment_of(o, "RECURRENCE-ID")?;
                let ostart = moment_of(o, "DTSTART").unwrap_or_else(|| aligned(&rid, &start));
                let mut row = base_row(o, &ostart, &doc, phone);
                single_end(&mut row, &end_of(o, &ostart), &doc, phone);
                row.original_instance = Some(android_time(&aligned(&rid, &start), &doc, phone).0);
                row.original_all_day = Some(main.all_day);
                Some(row)
            })
            .collect()
    } else {
        Vec::new()
    };
    Some(EventRows { main, exceptions })
}

// ---------------------------------------------------------------------------
// Edits from Android.

/// An RRULE value by meaning: parts sorted, list values sorted, defaults
/// (`INTERVAL=1`, `WKST=MO`) dropped, case folded.
fn rule_meaning(r: Option<&str>) -> Option<Vec<(String, String)>> {
    let r = r?.lines().next()?.trim().trim_start_matches("RRULE:");
    if r.is_empty() {
        return None;
    }
    let mut parts: Vec<(String, String)> = r
        .split(';')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| {
            let k = k.trim().to_ascii_uppercase();
            let mut vs: Vec<String> = v.split(',').map(|x| x.trim().to_ascii_uppercase()).collect();
            vs.sort();
            (k, vs.join(","))
        })
        .filter(|(k, v)| !matches!((k.as_str(), v.as_str()), ("INTERVAL", "1") | ("WKST", "MO")))
        .collect();
    parts.sort();
    Some(parts)
}

fn same_text(a: &Option<String>, b: &Option<String>) -> bool {
    a.as_deref().unwrap_or("").trim() == b.as_deref().unwrap_or("").trim()
}

/// The change that turns `before` (what the bridge wrote) into `after`
/// (what Android holds now). `like` is the component's current start, so
/// a moved time keeps its kind.
fn change_between(
    before: &EventRow,
    after: &EventRow,
    like: &Moment,
    phone: chrono_tz::Tz,
    series: bool,
) -> EventChange {
    let mut c = EventChange::default();
    if !same_text(&before.title, &after.title) {
        c.summary = Some(after.title.clone().unwrap_or_default());
    }
    if !same_text(&before.location, &after.location) {
        c.location = Some(after.location.clone().unwrap_or_default());
    }
    if !same_text(&before.description, &after.description) {
        c.description = Some(after.description.clone().unwrap_or_default());
    }
    let (bs, be) = (before.dtstart, end_ms(before));
    let (as_, mut ae) = (after.dtstart, end_ms(after));
    if after.all_day && ae <= as_ {
        ae = as_ + 86_400_000;
    }
    if before.all_day != after.all_day || bs != as_ || be != ae {
        let start = moment_from(as_, &after.timezone, after.all_day, Some(like), phone);
        let end_tz = after.end_timezone.as_deref().filter(|_| !series).unwrap_or(&after.timezone);
        let end = moment_from(ae.max(as_), end_tz, after.all_day, Some(&start), phone);
        c.start = Some(start);
        c.end = Some(end);
    }
    if series && rule_meaning(before.rrule.as_deref()) != rule_meaning(after.rrule.as_deref()) {
        c.rrule = Some(
            after.rrule.as_deref().and_then(|r| r.lines().next()).map(str::to_string).filter(|r| !r.trim().is_empty()),
        );
    }
    let mut a = after.reminders.clone();
    a.sort_unstable();
    a.dedup();
    let mut b = before.reminders.clone();
    b.dedup();
    if a != b {
        c.alarms = Some(a);
    }
    c
}

fn is_empty(c: &EventChange) -> bool {
    *c == EventChange::default()
}

/// The instance key (the master's terms) of the occurrence Android knows by
/// `original` (UTC milliseconds): a moved occurrence by its RECURRENCE-ID,
/// else the expansion's occurrence starting there.
fn instance_key(text: &str, doc: &Document, ev: &Event<'_>, original: i64, phone: chrono_tz::Tz) -> Option<Key> {
    let start = moment_of(ev.master, "DTSTART")?;
    for o in &ev.overrides {
        let rid = moment_of(o, "RECURRENCE-ID")?;
        let a = aligned(&rid, &start);
        if android_time(&a, doc, phone).0 == original {
            return Some(a.key());
        }
    }
    let at = from_ms(original);
    // The key follows from the time for every start Android can name.
    match &start {
        Moment::Date(_) => return Some(Key::Date(at.date())),
        Moment::Utc(_) => return Some(Key::Time(at)),
        Moment::Floating(_) => return Some(Key::Time(utc_to_local(phone, at))),
        Moment::Zoned { tzid, .. } => {
            if let Some(z) = iana(tzid) {
                return Some(Key::Time(utc_to_local(z, at)));
            }
        }
    }
    // A zone only the object's VTIMEZONE knows: ask the expansion.
    let exp = recur::expand(text, at - Duration::seconds(1), at + Duration::seconds(1), phone).ok()?;
    exp.occurrences.iter().find_map(|o| match o.recurrence_id? {
        When::Zoned { local, utc } if utc == at => Some(Key::Time(local)),
        _ => None,
    })
}

/// Apply what Android holds for one row of a stored event. `edited` is a
/// series row (no `original_instance`) or an exception row. Returns the
/// new text, the same text when nothing changed by meaning, or `None` when
/// the event is gone.
pub fn apply(
    text: &str,
    edited: &EventRow,
    phone: chrono_tz::Tz,
    now: NaiveDateTime,
    new_uid: &str,
) -> Result<Option<String>, BridgeError> {
    let doc = Document::parse(text).map_err(|_| BridgeError::NotAnEvent)?;
    let ev = event(&doc).ok_or(BridgeError::NotAnEvent)?;
    let before = rows(text, phone).ok_or(BridgeError::NotAnEvent)?;
    let master_start = moment_of(ev.master, "DTSTART").ok_or(BridgeError::NotAnEvent)?;
    let series = before.main.rrule.is_some() || before.main.rdate.is_some();

    let Some(original) = edited.original_instance else {
        let mut text = text.to_string();
        // EXDATEs another app added (Fossify cancels exceptions instead).
        if series {
            let had: Vec<&str> = before.main.exdate.as_deref().map(|s| s.split(',').collect()).unwrap_or_default();
            for v in edited.exdate.as_deref().unwrap_or("").split(',').map(str::trim).filter(|v| !v.is_empty()) {
                if had.contains(&v) {
                    continue;
                }
                let utc = v.split(';').next_back().unwrap_or(v);
                let Some(t) = moment::parse_naive(utc.trim_end_matches('Z')) else { continue };
                let doc = Document::parse(&text).map_err(|_| BridgeError::NotAnEvent)?;
                let ev = event(&doc).ok_or(BridgeError::NotAnEvent)?;
                if let Some(key) = instance_key(&text, &doc, &ev, ms(t), phone) {
                    match edit::delete(&text, Some(key), Scope::This, now)?.updated {
                        Some(t) => text = t,
                        None => return Ok(None),
                    }
                }
            }
        }
        let change = change_between(&before.main, edited, &master_start, phone, series);
        if is_empty(&change) {
            return Ok(Some(text));
        }
        return Ok(edit::edit(&text, None, Scope::All, &change, now, new_uid)?.updated);
    };

    if !series {
        return Err(BridgeError::NoSuchInstance);
    }
    let key = instance_key(text, &doc, &ev, original, phone).ok_or(BridgeError::NoSuchInstance)?;
    if edited.status == Some(STATUS_CANCELED) {
        return Ok(edit::delete(text, Some(key), Scope::This, now)?.updated);
    }
    let (before_row, like) = match before.exceptions.iter().find(|e| e.original_instance == Some(original)) {
        Some(r) => {
            let o = ev
                .overrides
                .iter()
                .find(|o| {
                    moment_of(o, "RECURRENCE-ID")
                        .is_some_and(|rid| android_time(&aligned(&rid, &master_start), &doc, phone).0 == original)
                })
                .ok_or(BridgeError::NoSuchInstance)?;
            (r.clone(), moment_of(o, "DTSTART").unwrap_or_else(|| master_start.with_key(key)))
        }
        None => {
            // A new exception: the occurrence as the series has it.
            let len = end_ms(&before.main) - before.main.dtstart;
            let r = EventRow {
                dtstart: original,
                dtend: Some(original + len),
                duration: None,
                rrule: None,
                rdate: None,
                exrule: None,
                exdate: None,
                original_instance: Some(original),
                ..before.main.clone()
            };
            (r, master_start.with_key(key))
        }
    };
    let change = change_between(&before_row, edited, &like, phone, false);
    if is_empty(&change) {
        return Ok(Some(text.to_string()));
    }
    Ok(edit::edit(text, Some(key), Scope::This, &change, now, new_uid)?.updated)
}

/// A row another app made, as a new object. An empty title gets
/// `fallback_title` (the core needs one).
pub fn new_event(
    row: &EventRow,
    uid: &str,
    phone: chrono_tz::Tz,
    now: NaiveDateTime,
    fallback_title: &str,
) -> Result<String, BridgeError> {
    let start = moment_from(row.dtstart, &row.timezone, row.all_day, None, phone);
    let mut e = end_ms(row);
    if row.all_day && e <= row.dtstart {
        e = row.dtstart + 86_400_000;
    }
    let end = moment_from(e.max(row.dtstart), &row.timezone, row.all_day, Some(&start), phone);
    let title = row.title.as_deref().filter(|t| !t.trim().is_empty()).unwrap_or(fallback_title);
    let change = EventChange {
        summary: Some(title.to_string()),
        location: row.location.clone(),
        description: row.description.clone(),
        start: Some(start),
        end: Some(end),
        rrule: Some(
            row.rrule.as_deref().and_then(|r| r.lines().next()).map(str::to_string).filter(|r| !r.trim().is_empty()),
        ),
        alarms: (!row.reminders.is_empty()).then(|| row.reminders.clone()),
    };
    Ok(edit::new_event(&change, uid, now)?)
}

/// The phone's zone by name, UTC when Android names one the database does
/// not know.
pub fn zone(name: &str) -> chrono_tz::Tz {
    name.parse().unwrap_or(chrono_tz::UTC)
}

/// A date as Android's all-day start (UTC midnight), for tests and callers.
pub fn all_day_ms(d: NaiveDate) -> i64 {
    ms(d.and_hms_opt(0, 0, 0).expect("midnight"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const AMS: chrono_tz::Tz = chrono_tz::Europe::Amsterdam;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 9).unwrap().and_hms_opt(12, 0, 0).unwrap()
    }

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//t//EN\r\n{body}END:VCALENDAR\r\n")
    }

    fn weekly() -> String {
        cal("BEGIN:VEVENT\r\nUID:w\r\nDTSTAMP:20260101T000000Z\r\nDTSTART;TZID=Europe/Amsterdam:20260105T090000\r\n\
             DTEND;TZID=Europe/Amsterdam:20260105T100000\r\nRRULE:FREQ=WEEKLY;BYDAY=MO\r\nEXDATE;TZID=Europe/Amsterdam:20260112T090000\r\n\
             SUMMARY:Swim\r\nX-UNKNOWN;P=1:kept\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:x\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nUID:w\r\nDTSTAMP:20260101T000000Z\r\nRECURRENCE-ID;TZID=Europe/Amsterdam:20260119T090000\r\n\
             DTSTART;TZID=Europe/Amsterdam:20260119T110000\r\nDTEND;TZID=Europe/Amsterdam:20260119T120000\r\nSUMMARY:Swim late\r\nEND:VEVENT\r\n")
    }

    fn at(y: i32, m: u32, d: u32, h: u32, mi: u32) -> i64 {
        ms(NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, mi, 0).unwrap())
    }

    #[test]
    fn a_series_with_an_exception_and_an_exdate() {
        let r = rows(&weekly(), AMS).unwrap();
        assert_eq!(r.main.title.as_deref(), Some("Swim"));
        assert_eq!(r.main.dtstart, at(2026, 1, 5, 8, 0), "09:00 Amsterdam is 08:00 UTC in winter");
        assert_eq!(r.main.timezone, "Europe/Amsterdam");
        assert_eq!(r.main.dtend, None);
        assert_eq!(r.main.duration.as_deref(), Some("P3600S"), "not PT3600S: the provider crashes on it");
        assert_eq!(r.main.rrule.as_deref(), Some("FREQ=WEEKLY;BYDAY=MO"));
        assert_eq!(r.main.exdate.as_deref(), Some("20260112T080000Z"));
        assert_eq!(r.main.reminders, vec![15]);
        assert_eq!(r.exceptions.len(), 1);
        let x = &r.exceptions[0];
        assert_eq!(x.original_instance, Some(at(2026, 1, 19, 8, 0)));
        assert_eq!(x.dtstart, at(2026, 1, 19, 10, 0));
        assert_eq!(x.dtend, Some(at(2026, 1, 19, 11, 0)));
        assert_eq!(x.title.as_deref(), Some("Swim late"));
    }

    #[test]
    fn single_all_day_and_floating() {
        let all_day = cal("BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260310\r\nSUMMARY:Holiday\r\nEND:VEVENT\r\n");
        let r = rows(&all_day, AMS).unwrap().main;
        assert!(r.all_day);
        assert_eq!(
            (r.dtstart, r.dtend, r.timezone.as_str()),
            (at(2026, 3, 10, 0, 0), Some(at(2026, 3, 11, 0, 0)), "UTC")
        );
        let floating = cal(
            "BEGIN:VEVENT\r\nUID:f\r\nDTSTART:20260310T090000\r\nDTEND:20260310T093000\r\nSUMMARY:F\r\nEND:VEVENT\r\n",
        );
        let r = rows(&floating, AMS).unwrap().main;
        assert_eq!((r.dtstart, r.timezone.as_str()), (at(2026, 3, 10, 8, 0), "Europe/Amsterdam"));
        assert_eq!(r.dtend, Some(at(2026, 3, 10, 8, 30)));
        let task = cal("BEGIN:VTODO\r\nUID:t\r\nSUMMARY:x\r\nEND:VTODO\r\n");
        assert!(rows(&task, AMS).is_none());
    }

    #[test]
    fn nothing_changed_is_no_edit_even_in_fossifys_forms() {
        let text = weekly();
        let mut r = rows(&text, AMS).unwrap().main;
        assert_eq!(apply(&text, &r, AMS, now(), "n").unwrap().as_deref(), Some(text.as_str()));
        // Fossify rewrites a series in equal forms on any edit (decision 45).
        r.rrule = Some("FREQ=WEEKLY;INTERVAL=1;BYDAY=MO;WKST=MO".into());
        r.duration = Some("P0DT1H0M0S".into());
        assert_eq!(apply(&text, &r, AMS, now(), "n").unwrap().as_deref(), Some(text.as_str()));
        let x = rows(&text, AMS).unwrap().exceptions[0].clone();
        assert_eq!(apply(&text, &x, AMS, now(), "n").unwrap().as_deref(), Some(text.as_str()));
    }

    #[test]
    fn a_title_edit_changes_only_the_title() {
        let text = weekly();
        let mut r = rows(&text, AMS).unwrap().main;
        r.title = Some("Swimming".into());
        let out = apply(&text, &r, AMS, now(), "n").unwrap().unwrap();
        assert!(out.contains("SUMMARY:Swimming\r\n") && out.contains("X-UNKNOWN;P=1:kept"));
        assert!(out.contains("RRULE:FREQ=WEEKLY;BYDAY=MO") && out.contains("SUMMARY:Swim late"));
        assert_eq!(rows(&out, AMS).unwrap().main.dtstart, r.dtstart);
    }

    #[test]
    fn moving_one_occurrence_and_canceling_another() {
        let text = weekly();
        let main = rows(&text, AMS).unwrap().main;
        // Fossify: a new exception row for the 26th, an hour later.
        let moved = EventRow {
            dtstart: at(2026, 1, 26, 9, 0),
            dtend: Some(at(2026, 1, 26, 10, 0)),
            duration: None,
            rrule: None,
            exdate: None,
            original_instance: Some(at(2026, 1, 26, 8, 0)),
            ..main.clone()
        };
        let out = apply(&text, &moved, AMS, now(), "n").unwrap().unwrap();
        assert!(out.contains("RECURRENCE-ID;TZID=Europe/Amsterdam:20260126T090000"), "{out}");
        assert!(out.contains("DTSTART;TZID=Europe/Amsterdam:20260126T100000"), "{out}");
        let after = rows(&out, AMS).unwrap();
        assert_eq!(after.exceptions.len(), 2);
        // Fossify: one occurrence deleted is a canceled exception row.
        let canceled = EventRow {
            status: Some(STATUS_CANCELED),
            title: None,
            original_instance: Some(at(2026, 2, 2, 8, 0)),
            ..moved
        };
        let out = apply(&out, &canceled, AMS, now(), "n").unwrap().unwrap();
        assert!(rows(&out, AMS).unwrap().main.exdate.unwrap().contains("20260202T080000Z"));
    }

    #[test]
    fn times_reminders_and_rules() {
        let text = weekly();
        let mut r = rows(&text, AMS).unwrap().main;
        r.dtstart = at(2026, 1, 5, 9, 0);
        r.reminders = vec![30, 0];
        r.rrule = Some("FREQ=WEEKLY;BYDAY=MO;COUNT=10".into());
        let out = apply(&text, &r, AMS, now(), "n").unwrap().unwrap();
        assert!(out.contains("DTSTART;TZID=Europe/Amsterdam:20260105T100000"), "{out}");
        assert!(out.contains("DTEND;TZID=Europe/Amsterdam:20260105T110000"), "{out}");
        assert!(out.contains("COUNT=10") && out.contains("TRIGGER:-PT30M") && !out.contains("TRIGGER:-PT15M"));
        r.rrule = None;
        let out = apply(&text, &r, AMS, now(), "n").unwrap().unwrap();
        assert!(!out.contains("RRULE:FREQ=WEEKLY"), "the series no longer repeats (the VTIMEZONE keeps its own rules)");
    }

    #[test]
    fn floating_stays_floating_and_all_day_stays_a_date() {
        let floating = cal(
            "BEGIN:VEVENT\r\nUID:f\r\nDTSTART:20260310T090000\r\nDTEND:20260310T093000\r\nSUMMARY:F\r\nEND:VEVENT\r\n",
        );
        let mut r = rows(&floating, AMS).unwrap().main;
        r.dtstart += 3_600_000;
        r.dtend = r.dtend.map(|e| e + 3_600_000);
        let out = apply(&floating, &r, AMS, now(), "n").unwrap().unwrap();
        assert!(out.contains("DTSTART:20260310T100000\r\n"), "{out}");
        let all_day = cal(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20260310\r\nDTEND;VALUE=DATE:20260311\r\nSUMMARY:H\r\nEND:VEVENT\r\n",
        );
        let mut r = rows(&all_day, AMS).unwrap().main;
        r.dtstart = all_day_ms(NaiveDate::from_ymd_opt(2026, 3, 12).unwrap());
        r.dtend = Some(r.dtstart);
        let out = apply(&all_day, &r, AMS, now(), "n").unwrap().unwrap();
        assert!(out.contains("DTSTART;VALUE=DATE:20260312") && out.contains("DTEND;VALUE=DATE:20260313"), "{out}");
    }

    #[test]
    fn new_events_from_another_app() {
        let row = EventRow {
            title: Some(" ".into()),
            dtstart: at(2026, 3, 10, 8, 0),
            dtend: Some(at(2026, 3, 10, 8, 0)),
            timezone: "Europe/Amsterdam".into(),
            rrule: Some("FREQ=DAILY;COUNT=3".into()),
            reminders: vec![10],
            ..EventRow::default()
        };
        let text = new_event(&row, "u-1", AMS, now(), "(untitled)").unwrap();
        assert!(text.contains("UID:u-1") && text.contains("SUMMARY:(untitled)"));
        assert!(
            text.contains("DTSTART;TZID=Europe/Amsterdam:20260310T090000"),
            "an event may end when it starts: {text}"
        );
        assert!(text.contains("RRULE:FREQ=DAILY;COUNT=3") && text.contains("TRIGGER:-PT10M"));
        let back = rows(&text, AMS).unwrap().main;
        assert_eq!(back.dtstart, row.dtstart);
    }

    #[test]
    fn rdate_and_android_durations() {
        let text = cal(
            "BEGIN:VEVENT\r\nUID:r\r\nDTSTART;VALUE=DATE:20260301\r\nRDATE;VALUE=DATE:20260305,20260310\r\nSUMMARY:R\r\nEND:VEVENT\r\n",
        );
        let m = rows(&text, AMS).unwrap().main;
        assert_eq!(m.rdate.as_deref(), Some("20260301T000000Z,20260305T000000Z,20260310T000000Z"));
        assert_eq!(m.duration.as_deref(), Some("P1D"));
        let endless = cal(
            "BEGIN:VEVENT\r\nUID:e\r\nDTSTART:20260301T090000Z\r\nRRULE:FREQ=DAILY\r\nRDATE:20260305T120000Z\r\nSUMMARY:E\r\nEND:VEVENT\r\n",
        );
        assert_eq!(rows(&endless, AMS).unwrap().main.rdate, None, "the provider cannot expand both");
        assert_eq!(parse_android_duration("P3600S"), Some(Duration::hours(1)));
        assert_eq!(parse_android_duration("P0DT1H0M0S"), Some(Duration::hours(1)));
        assert_eq!(parse_android_duration("P1W"), Some(Duration::weeks(1)));
        assert_eq!(parse_android_duration("PT"), None);
    }
}
