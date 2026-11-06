//! Editing events (phase 2 step 1): a change set applied to an event's
//! content-line tree. Only the lines a change touches are rewritten, so
//! everything else stays byte for byte as the client wrote it (decision
//! 25). Recurring events offer all three scopes (decision 12):
//!
//! - **This:** an override (`RECURRENCE-ID`) for the one instance; deleting
//!   adds an `EXDATE`.
//! - **Following:** the series ends just before the instance (`UNTIL`; a
//!   `COUNT` becomes an `UNTIL`), and a new series with a new UID starts
//!   there, taking the later overrides and exceptions along.
//! - **All:** the master changes; overrides and exceptions move with a
//!   time shift, and a title or place an override shared with the master
//!   follows it.
//!
//! The text given is the object as the editing person reads it (their own
//! alarms merged in), so reminders set here land in their layer again when
//! the server stores it.

use chrono::{Duration, NaiveDateTime};

use crate::lines::{Component, Document, Item, Line};
use crate::moment::{self, Key, Moment};
use crate::recur;

/// What to change; `None` leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventChange {
    pub summary: Option<String>,
    /// Empty clears it.
    pub location: Option<String>,
    /// Empty clears it.
    pub description: Option<String>,
    pub start: Option<Moment>,
    /// Exclusive for all-day events (the day after the last day). When the
    /// start moves and this is `None`, the length stays the same.
    pub end: Option<Moment>,
    /// `Some(None)`: no longer repeats. The value is an RRULE value
    /// (`FREQ=WEEKLY;BYDAY=MO`).
    pub rrule: Option<Option<String>>,
    /// Reminders as minutes before the start, replacing the editing
    /// person's own (their layer). `Some(vec![])` removes them.
    pub alarms: Option<Vec<i64>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    This,
    Following,
    All,
}

/// The result of an edit or delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// The object's new text; `None` when it is to be deleted.
    pub updated: Option<String>,
    /// A new object (a "this and following" split): its text.
    pub created: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("not an event")]
    NotAnEvent,
    #[error("the event has no start")]
    NoStart,
    #[error("no such occurrence")]
    NoSuchInstance,
    #[error("the end is before the start")]
    EndBeforeStart,
    #[error("a new event needs a title and a start")]
    Incomplete,
    #[error("not a repeat rule: {0}")]
    BadRule(String),
}

const PRODID: &str = "PRODID:-//CalZone//CalZone//EN";

fn stamp(t: NaiveDateTime) -> String {
    format!("{}Z", t.format("%Y%m%dT%H%M%S"))
}

/// RFC 5545 TEXT escaping.
pub fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            ';' => out.push_str("\\;"),
            ',' => out.push_str("\\,"),
            '\n' => out.push_str("\\n"),
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// A new event. `now` is UTC; `uid` must be fresh.
pub fn new_event(change: &EventChange, uid: &str, now: NaiveDateTime) -> Result<String, EditError> {
    let (Some(summary), Some(start)) = (change.summary.as_deref().filter(|s| !s.trim().is_empty()), &change.start)
    else {
        return Err(EditError::Incomplete);
    };
    let end = change.end.clone().unwrap_or_else(|| default_end(start));
    check_order(start, &end)?;
    let mut ev = Component::new("VEVENT");
    let mut props = vec![
        Line::new(format!("UID:{uid}")),
        Line::new(format!("DTSTAMP:{}", stamp(now))),
        Line::new(format!("CREATED:{}", stamp(now))),
        Line::new(format!("LAST-MODIFIED:{}", stamp(now))),
        Line::new(format!("SUMMARY:{}", escape_text(summary.trim()))),
        start.to_line("DTSTART"),
        end.to_line("DTEND"),
    ];
    for (name, v) in [("LOCATION", &change.location), ("DESCRIPTION", &change.description)] {
        if let Some(v) = v.as_deref().filter(|v| !v.trim().is_empty()) {
            props.push(Line::new(format!("{name}:{}", escape_text(v))));
        }
    }
    if let Some(Some(rule)) = &change.rrule {
        props.push(Line::new(format!("RRULE:{}", check_rule(rule)?)));
    }
    ev.items.extend(props.into_iter().map(Item::Property));
    if let Some(alarms) = &change.alarms {
        ev.items.extend(alarms.iter().map(|m| Item::Component(alarm(*m))));
    }
    let mut cal = Component::new("VCALENDAR");
    cal.items.push(Item::Property(Line::new("VERSION:2.0")));
    cal.items.push(Item::Property(Line::new(PRODID)));
    cal.items.push(Item::Component(ev));
    let mut doc = Document { components: vec![cal], trailing: String::new() };
    for m in [start, &end] {
        if let Moment::Zoned { tzid, local } = m {
            moment::ensure_zone(&mut doc, tzid, chrono::Datelike::year(local));
        }
    }
    Ok(doc.to_string())
}

fn default_end(start: &Moment) -> Moment {
    if start.is_date() { start.shifted(Duration::days(1)) } else { start.shifted(Duration::hours(1)) }
}

fn check_order(start: &Moment, end: &Moment) -> Result<(), EditError> {
    let ok = match (start.key(), end.key()) {
        (Key::Date(s), Key::Date(e)) => e > s,
        (Key::Time(s), Key::Time(e)) => e >= s,
        _ => false,
    };
    if ok { Ok(()) } else { Err(EditError::EndBeforeStart) }
}

/// A light check of an RRULE value: `FREQ` present, every part `KEY=VALUE`.
fn check_rule(rule: &str) -> Result<String, EditError> {
    let rule = rule.trim().trim_start_matches("RRULE:");
    let parts: Vec<&str> = rule.split(';').filter(|p| !p.is_empty()).collect();
    let well_formed = parts.iter().all(|p| p.split_once('=').is_some_and(|(k, v)| !k.is_empty() && !v.is_empty()));
    let freq = parts.iter().any(|p| {
        p.split_once('=').is_some_and(|(k, v)| {
            k.eq_ignore_ascii_case("FREQ")
                && matches!(
                    v.to_ascii_uppercase().as_str(),
                    "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY" | "HOURLY" | "MINUTELY" | "SECONDLY"
                )
        })
    });
    if well_formed && freq {
        Ok(parts.join(";").to_ascii_uppercase())
    } else {
        Err(EditError::BadRule(rule.to_string()))
    }
}

/// A display reminder `minutes` before the start.
fn alarm(minutes: i64) -> Component {
    let trigger = match minutes {
        0 => "-PT0M".to_string(),
        m if m > 0 && m % 1440 == 0 => format!("-P{}D", m / 1440),
        m if m > 0 && m % 60 == 0 => format!("-PT{}H", m / 60),
        m if m > 0 => format!("-PT{m}M"),
        m => format!("PT{}M", -m),
    };
    let mut a = Component::new("VALARM");
    for l in ["ACTION:DISPLAY".to_string(), "DESCRIPTION:Reminder".to_string(), format!("TRIGGER:{trigger}")] {
        a.items.push(Item::Property(Line::new(l)));
    }
    a
}

// ---------------------------------------------------------------------------
// Property helpers on one component.

fn props_named<'a>(c: &'a Component, name: &'a str) -> impl Iterator<Item = &'a Line> {
    c.properties().filter(move |l| l.name() == name)
}

/// Replace the first `name` property (removing any others), or remove them
/// all when `line` is `None`. A new property goes after the last property,
/// before the first sub-component.
fn set_prop(c: &mut Component, name: &str, line: Option<Line>) {
    let first = c.items.iter().position(|i| matches!(i, Item::Property(l) if l.name() == name));
    let mut line = line;
    let mut idx = 0;
    c.items.retain(|i| {
        let keep = match i {
            Item::Property(l) if l.name() == name => Some(idx) == first && line.is_some(),
            _ => true,
        };
        idx += 1;
        keep
    });
    match (first, line.take()) {
        (Some(at), Some(l)) => {
            // `at` still points at the kept line: nothing before it was removed.
            c.items[at] = Item::Property(l);
        }
        (None, Some(l)) => {
            let at = c.items.iter().position(|i| matches!(i, Item::Component(_))).unwrap_or(c.items.len());
            c.items.insert(at, Item::Property(l));
        }
        _ => {}
    }
}

fn remove_props(c: &mut Component, name: &str) {
    set_prop(c, name, None);
}

fn moment_of(c: &Component, name: &str) -> Option<Moment> {
    c.property(name).and_then(Moment::from_line)
}

fn rid_of(c: &Component) -> Option<Moment> {
    moment_of(c, "RECURRENCE-ID")
}

fn is_recurring(c: &Component) -> bool {
    c.property("RRULE").is_some() || c.property("RDATE").is_some()
}

/// The end as stated (DTEND, or DTSTART + DURATION), else the RFC default.
fn end_of(c: &Component, start: &Moment) -> Moment {
    if let Some(e) = moment_of(c, "DTEND") {
        return e;
    }
    if let Some(d) = c.property("DURATION").and_then(|l| parse_duration(l.value())) {
        return start.shifted(d);
    }
    if start.is_date() { start.shifted(Duration::days(1)) } else { start.clone() }
}

/// `P1D`, `PT1H30M`, `-PT15M` (RFC 5545 3.3.6).
pub fn parse_duration(v: &str) -> Option<Duration> {
    let v = v.trim();
    let (neg, v) = match v.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, v.strip_prefix('+').unwrap_or(v)),
    };
    let v = v.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut in_time = false;
    for ch in v.chars() {
        match ch {
            'T' => in_time = true,
            '0'..='9' => num.push(ch),
            u => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (u, in_time) {
                    ('W', _) => Duration::weeks(n),
                    ('D', _) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(if neg { -total } else { total })
}

fn length(start: &Moment, end: &Moment) -> Duration {
    match (start.key(), end.key()) {
        (Key::Date(s), Key::Date(e)) => e - s,
        (Key::Time(s), Key::Time(e)) => e - s,
        _ => Duration::zero(),
    }
}

/// An instant to compare instances with: dates as dates, times in UTC
/// (zoned through their zone), floating as written.
fn instant(m: &Moment, doc: &Document) -> Key {
    match m {
        Moment::Date(d) => Key::Date(*d),
        Moment::Floating(t) | Moment::Utc(t) => Key::Time(*t),
        Moment::Zoned { local, tzid } => Key::Time(moment::zoned_to_utc(*local, tzid, doc)),
    }
}

// ---------------------------------------------------------------------------
// Multi-valued EXDATE / RDATE lines.

/// The values of every `name` line with their line head (`EXDATE;TZID=X`).
fn date_list(c: &Component, name: &str) -> Vec<(String, Moment)> {
    let mut out = Vec::new();
    for l in props_named(c, name) {
        let head = l.text().split_once(':').map(|(h, _)| h.to_string()).unwrap_or_default();
        for v in l.value().split(',') {
            let single = Line::new(format!("{head}:{v}"));
            if let Some(m) = Moment::from_line(&single) {
                out.push((head.clone(), m));
            }
        }
    }
    out
}

/// Rewrite `name` lines from values; untouched when nothing changed.
fn put_date_list(c: &mut Component, name: &str, values: &[(String, Moment)]) {
    remove_props(c, name);
    let at = c.items.iter().position(|i| matches!(i, Item::Component(_))).unwrap_or(c.items.len());
    let lines: Vec<Item> = values.iter().map(|(_, m)| Item::Property(m.to_line(name))).collect();
    for (k, l) in lines.into_iter().enumerate() {
        c.items.insert(at + k, l);
    }
}

// ---------------------------------------------------------------------------
// RRULE parts.

fn rule_parts(c: &Component) -> Vec<(String, String)> {
    c.property("RRULE")
        .map(|l| {
            l.value()
                .split(';')
                .filter_map(|p| p.split_once('=').map(|(k, v)| (k.to_ascii_uppercase(), v.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

fn set_rule_parts(c: &mut Component, parts: &[(String, String)]) {
    let v: Vec<String> = parts.iter().map(|(k, v)| format!("{k}={v}")).collect();
    set_prop(c, "RRULE", Some(Line::new(format!("RRULE:{}", v.join(";")))));
}

/// End a series just before `split` (in the master's terms): a `COUNT`
/// becomes an `UNTIL`.
fn end_series_before(master: &mut Component, split: &Moment, doc: &Document) {
    let until = match split {
        Moment::Date(d) => Moment::Date(*d - Duration::days(1)),
        other => other.shifted(Duration::seconds(-1)),
    };
    let mut parts: Vec<(String, String)> =
        rule_parts(master).into_iter().filter(|(k, _)| k != "COUNT" && k != "UNTIL").collect();
    parts.push(("UNTIL".into(), until.until_value(doc)));
    set_rule_parts(master, &parts);
}

/// How many instances the rule alone (no exceptions) makes before `split`.
fn instances_before(doc: &Document, master: &Component, split: &Moment) -> usize {
    let mut ev = Component::new("VEVENT");
    for name in ["UID", "DTSTART", "DTEND", "DURATION", "RRULE"] {
        if let Some(l) = master.property(name) {
            ev.items.push(Item::Property(l.clone()));
        }
    }
    let mut cal = Component::new("VCALENDAR");
    cal.items.push(Item::Property(Line::new("VERSION:2.0")));
    if let Some(src) = doc.components.first() {
        cal.items.extend(src.components().filter(|z| z.name == "VTIMEZONE").cloned().map(Item::Component));
    }
    cal.items.push(Item::Component(ev));
    let text = Document { components: vec![cal], trailing: String::new() }.to_string();
    let as_time = |k: Key| match k {
        Key::Time(t) => t,
        Key::Date(d) => d.and_hms_opt(0, 0, 0).expect("midnight"),
    };
    let to = as_time(instant(split, doc));
    let Some(first) = moment_of(master, "DTSTART") else { return 0 };
    let from = as_time(instant(&first, doc)) - Duration::days(1);
    match recur::expand(&text, from, to, chrono_tz::UTC) {
        Ok(e) => e.occurrences.iter().filter(|o| o.start.instant(chrono_tz::UTC) < to).count(),
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// Applying a change to one component.

struct Applied {
    /// How far the start moved, when its kind stayed the same.
    shift: Option<Duration>,
    kind_changed: bool,
}

fn apply(
    c: &mut Component,
    change: &EventChange,
    now: NaiveDateTime,
    zones: &mut Vec<(String, i32)>,
) -> Result<Applied, EditError> {
    let old_start = moment_of(c, "DTSTART").ok_or(EditError::NoStart)?;
    let old_end = end_of(c, &old_start);
    let new_start = change.start.clone().unwrap_or_else(|| old_start.clone());
    let kind_changed = new_start.is_date() != old_start.is_date();
    let new_end = match &change.end {
        Some(e) => e.clone(),
        None if change.start.is_none() => old_end.clone(),
        None if kind_changed => default_end(&new_start),
        None => new_start.shifted(length(&old_start, &old_end)),
    };
    check_order(&new_start, &new_end)?;
    let mut significant = false;
    // A VTIMEZONE is added only for a time this edit writes.
    let mut wrote = Vec::new();
    if new_start != old_start {
        set_prop(c, "DTSTART", Some(new_start.to_line("DTSTART")));
        significant = true;
        wrote.push(&new_start);
    }
    if new_end != old_end || c.property("DURATION").is_some() && change.end.is_some() {
        remove_props(c, "DURATION");
        set_prop(c, "DTEND", Some(new_end.to_line("DTEND")));
        significant = true;
        wrote.push(&new_end);
    }
    for m in wrote {
        if let Moment::Zoned { tzid, local } = m {
            zones.push((tzid.clone(), chrono::Datelike::year(local)));
        }
    }
    for (name, v) in
        [("SUMMARY", &change.summary), ("LOCATION", &change.location), ("DESCRIPTION", &change.description)]
    {
        if let Some(v) = v {
            let current = c.property(name).map(|l| crate::object::unescape(l.value()));
            if current.as_deref() == Some(v.as_str()) || current.is_none() && v.trim().is_empty() {
                continue;
            }
            let line = (!v.trim().is_empty()).then(|| Line::new(format!("{name}:{}", escape_text(v))));
            set_prop(c, name, line);
        }
    }
    if let Some(rule) = &change.rrule {
        match rule {
            Some(r) => {
                let r = check_rule(r)?;
                if c.property("RRULE").map(|l| l.value().to_ascii_uppercase()) != Some(r.clone()) {
                    set_prop(c, "RRULE", Some(Line::new(format!("RRULE:{r}"))));
                    significant = true;
                }
            }
            None => {
                if is_recurring(c) {
                    for n in ["RRULE", "RDATE", "EXDATE"] {
                        remove_props(c, n);
                    }
                    significant = true;
                }
            }
        }
    }
    if let Some(alarms) = &change.alarms {
        c.items.retain(|i| !matches!(i, Item::Component(a) if a.name == "VALARM"));
        c.items.extend(alarms.iter().map(|m| Item::Component(alarm(*m))));
    }
    set_prop(c, "DTSTAMP", Some(Line::new(format!("DTSTAMP:{}", stamp(now)))));
    set_prop(c, "LAST-MODIFIED", Some(Line::new(format!("LAST-MODIFIED:{}", stamp(now)))));
    if significant {
        let seq = c.property("SEQUENCE").and_then(|l| l.value().trim().parse::<i64>().ok()).unwrap_or(0);
        set_prop(c, "SEQUENCE", Some(Line::new(format!("SEQUENCE:{}", seq + 1))));
    }
    let shift = (!kind_changed && new_start != old_start).then(|| match (old_start.key(), new_start.key()) {
        (Key::Date(a), Key::Date(b)) => b - a,
        (Key::Time(a), Key::Time(b)) => b - a,
        _ => Duration::zero(),
    });
    Ok(Applied { shift, kind_changed })
}

/// Touch only the stamps and SEQUENCE (deletes of instances).
fn restamp(c: &mut Component, now: NaiveDateTime) {
    set_prop(c, "DTSTAMP", Some(Line::new(format!("DTSTAMP:{}", stamp(now)))));
    set_prop(c, "LAST-MODIFIED", Some(Line::new(format!("LAST-MODIFIED:{}", stamp(now)))));
    let seq = c.property("SEQUENCE").and_then(|l| l.value().trim().parse::<i64>().ok()).unwrap_or(0);
    set_prop(c, "SEQUENCE", Some(Line::new(format!("SEQUENCE:{}", seq + 1))));
}

// ---------------------------------------------------------------------------
// The object.

struct Parsed {
    doc: Document,
    /// Index into the calendar's items of each event component.
    events: Vec<usize>,
    master: Option<usize>,
}

fn parse(text: &str) -> Result<Parsed, EditError> {
    let doc = Document::parse(text).map_err(|_| EditError::NotAnEvent)?;
    let cal = doc.components.first().filter(|c| c.name == "VCALENDAR").ok_or(EditError::NotAnEvent)?;
    let events: Vec<usize> = cal
        .items
        .iter()
        .enumerate()
        .filter(|(_, i)| matches!(i, Item::Component(c) if c.name == "VEVENT"))
        .map(|(n, _)| n)
        .collect();
    if events.is_empty() {
        return Err(EditError::NotAnEvent);
    }
    let master = events.iter().copied().find(|&n| matches!(&cal.items[n], Item::Component(c) if rid_of(c).is_none()));
    Ok(Parsed { doc, events, master })
}

fn comp(doc: &Document, n: usize) -> &Component {
    match &doc.components[0].items[n] {
        Item::Component(c) => c,
        Item::Property(_) => unreachable!("an event index"),
    }
}

fn comp_mut(doc: &mut Document, n: usize) -> &mut Component {
    match &mut doc.components[0].items[n] {
        Item::Component(c) => c,
        Item::Property(_) => unreachable!("an event index"),
    }
}

/// Whether `key` (master terms) is a real instance of the series.
fn instance_exists(text: &str, master_start: &Moment, key: Key, doc: &Document) -> bool {
    let m = master_start.with_key(key);
    let at = match instant(&m, doc) {
        Key::Time(t) => t,
        Key::Date(d) => d.and_hms_opt(0, 0, 0).expect("midnight"),
    };
    let Ok(e) = recur::expand(text, at - Duration::days(2), at + Duration::days(2), chrono_tz::UTC) else {
        return false;
    };
    e.occurrences.iter().any(|o| {
        o.recurrence_id.is_some_and(|r| {
            let k = match r {
                recur::When::Date(d) => Key::Date(d),
                recur::When::Utc(t) | recur::When::Floating(t) => Key::Time(t),
                recur::When::Zoned { local, .. } => Key::Time(local),
            };
            k == key
        })
    })
}

fn finish(mut doc: Document, zones: &[(String, i32)]) -> String {
    for (tzid, year) in zones {
        moment::ensure_zone(&mut doc, tzid, *year);
    }
    doc.to_string()
}

/// Edit an event. `instance` names an occurrence of a recurring event (its
/// original start in the master's terms); `None` edits the whole event.
/// `new_uid` is used only for a "this and following" split.
pub fn edit(
    text: &str,
    instance: Option<Key>,
    scope: Scope,
    change: &EventChange,
    now: NaiveDateTime,
    new_uid: &str,
) -> Result<Outcome, EditError> {
    let Parsed { mut doc, events, master } = parse(text)?;
    let mut zones = Vec::new();
    let recurring = master.is_some_and(|m| is_recurring(comp(&doc, m)));
    let (Some(key), true) = (instance, recurring) else {
        // A single event, or the whole series.
        let m = master.unwrap_or(events[0]);
        return edit_all(doc, m, &events, change, now, zones);
    };
    let m = master.expect("recurring has a master");
    let master_start = moment_of(comp(&doc, m), "DTSTART").ok_or(EditError::NoStart)?;
    if !instance_exists(text, &master_start, key, &doc) {
        return Err(EditError::NoSuchInstance);
    }
    match scope {
        Scope::All => {
            let change = rebase(&doc, m, &events, &master_start, key, change);
            edit_all(doc, m, &events, &change, now, zones)
        }
        Scope::This => {
            let target = master_start.with_key(key);
            let existing = events.iter().copied().find(|&n| {
                Some(n) != master && rid_of(comp(&doc, n)).is_some_and(|r| instant(&r, &doc) == instant(&target, &doc))
            });
            match existing {
                Some(n) => {
                    apply(comp_mut(&mut doc, n), change, now, &mut zones)?;
                }
                None => {
                    let mc = comp(&doc, m);
                    let length = length(&master_start, &end_of(mc, &master_start));
                    let mut o = mc.clone();
                    for name in ["RRULE", "RDATE", "EXDATE", "RECURRENCE-ID", "DURATION", "CREATED"] {
                        remove_props(&mut o, name);
                    }
                    set_prop(&mut o, "DTSTART", Some(target.to_line("DTSTART")));
                    set_prop(&mut o, "DTEND", Some(target.shifted(length).to_line("DTEND")));
                    set_prop(&mut o, "RECURRENCE-ID", Some(target.to_line("RECURRENCE-ID")));
                    apply(&mut o, change, now, &mut zones)?;
                    let last = *events.last().expect("events");
                    doc.components[0].items.insert(last + 1, Item::Component(o));
                }
            }
            Ok(Outcome { updated: Some(finish(doc, &zones)), created: None })
        }
        Scope::Following => {
            let split = master_start.with_key(key);
            if instant(&split, &doc) <= instant(&master_start, &doc) {
                return edit_all(doc, m, &events, change, now, zones);
            }
            // The new series: the master from here on.
            let mc = comp(&doc, m).clone();
            let count = rule_parts(&mc).iter().find(|(k, _)| k == "COUNT").and_then(|(_, v)| v.parse::<usize>().ok());
            let before = count.map(|_| instances_before(&doc, &mc, &split));
            let length = length(&master_start, &end_of(&mc, &master_start));
            let mut n = mc.clone();
            set_prop(&mut n, "UID", Some(Line::new(format!("UID:{new_uid}"))));
            set_prop(&mut n, "DTSTART", Some(split.to_line("DTSTART")));
            remove_props(&mut n, "DURATION");
            set_prop(&mut n, "DTEND", Some(split.shifted(length).to_line("DTEND")));
            set_prop(&mut n, "CREATED", Some(Line::new(format!("CREATED:{}", stamp(now)))));
            remove_props(&mut n, "SEQUENCE");
            if let (Some(total), Some(before)) = (count, before) {
                let left = total.saturating_sub(before);
                if left == 0 {
                    return Err(EditError::NoSuchInstance);
                }
                let parts: Vec<(String, String)> = rule_parts(&n)
                    .into_iter()
                    .map(|(k, v)| if k == "COUNT" { (k, left.to_string()) } else { (k, v) })
                    .collect();
                set_rule_parts(&mut n, &parts);
            }
            let split_at = instant(&split, &doc);
            for name in ["EXDATE", "RDATE"] {
                let all = date_list(&mc, name);
                if all.is_empty() {
                    continue;
                }
                let (early, late): (Vec<_>, Vec<_>) = all.into_iter().partition(|(_, v)| instant(v, &doc) < split_at);
                put_date_list(comp_mut(&mut doc, m), name, &early);
                put_date_list(&mut n, name, &late);
            }
            let snapshot = doc.clone();
            end_series_before(comp_mut(&mut doc, m), &split, &snapshot);
            restamp(comp_mut(&mut doc, m), now);
            // Later overrides move to the new series.
            let moving: Vec<usize> = events
                .iter()
                .copied()
                .filter(|&k| Some(k) != master && rid_of(comp(&doc, k)).is_some_and(|r| instant(&r, &doc) >= split_at))
                .collect();
            let mut moved: Vec<Component> = moving.iter().map(|&k| comp(&doc, k).clone()).collect();
            for k in moving.iter().rev() {
                doc.components[0].items.remove(*k);
            }
            let applied = apply(&mut n, change, now, &mut zones)?;
            for o in &mut moved {
                set_prop(o, "UID", Some(Line::new(format!("UID:{new_uid}"))));
            }
            reattach(&mut n, &mut moved, &applied);
            // The new object: the calendar's properties and zones, then the series.
            let mut cal = doc.components[0].clone();
            cal.items.retain(|i| match i {
                Item::Property(_) => true,
                Item::Component(c) => c.name == "VTIMEZONE",
            });
            cal.items.push(Item::Component(n));
            cal.items.extend(moved.into_iter().map(Item::Component));
            let created = finish(Document { components: vec![cal], trailing: doc.trailing.clone() }, &zones);
            Ok(Outcome { updated: Some(finish(doc, &zones)), created: Some(created) })
        }
    }
}

/// "All of them", asked on one occurrence: a new start or end is where that
/// occurrence goes, so the series moves by the same amount (dragging the
/// third Monday an hour later moves every Monday an hour later). A change
/// between all-day and timed keeps the day offset.
fn rebase(
    doc: &Document,
    m: usize,
    events: &[usize],
    master_start: &Moment,
    key: Key,
    change: &EventChange,
) -> EventChange {
    if change.start.is_none() && change.end.is_none() {
        return change.clone();
    }
    let master = comp(doc, m);
    let master_end = end_of(master, master_start);
    let target = master_start.with_key(key);
    let over = events
        .iter()
        .map(|&n| comp(doc, n))
        .find(|c| rid_of(c).is_some_and(|r| instant(&r, doc) == instant(&target, doc)));
    let inst_start = over.and_then(|c| moment_of(c, "DTSTART")).unwrap_or_else(|| target.clone());
    let inst_end =
        over.map(|c| end_of(c, &inst_start)).unwrap_or_else(|| inst_start.shifted(length(master_start, &master_end)));
    let day = |m: &Moment| match m.key() {
        Key::Date(d) => d,
        Key::Time(t) => t.date(),
    };
    // Where `base` (the master's start or end) goes when `inst` (the
    // occurrence's) goes to `new`.
    let move_like = |new: &Moment, inst: &Moment, base: &Moment| -> Moment {
        if new.is_date() == inst.is_date() {
            let delta = length(inst, new);
            if base.is_date() == new.is_date() { base.shifted(delta) } else { new.clone() }
        } else {
            let days = day(base) - day(inst);
            match new.key() {
                Key::Date(d) => Moment::Date(d + days),
                Key::Time(t) => new.with_key(Key::Time(t + days)),
            }
        }
    };
    EventChange {
        start: change.start.as_ref().map(|n| move_like(n, &inst_start, master_start)),
        end: change.end.as_ref().map(|n| move_like(n, &inst_end, &master_end)),
        ..change.clone()
    }
}

/// After the master moved: exceptions and override ids follow a shift (an
/// override that was never moved in time moves too), and both are dropped
/// when the event changed between all-day and timed (they could no longer
/// match an instance).
fn reattach(master: &mut Component, overrides: &mut Vec<Component>, applied: &Applied) {
    if applied.kind_changed {
        overrides.clear();
        for n in ["EXDATE", "RDATE"] {
            remove_props(master, n);
        }
        return;
    }
    let Some(shift) = applied.shift.filter(|s| !s.is_zero()) else { return };
    for n in ["EXDATE", "RDATE"] {
        let list = date_list(master, n);
        if !list.is_empty() {
            let moved: Vec<(String, Moment)> = list.into_iter().map(|(h, m)| (h, m.shifted(shift))).collect();
            put_date_list(master, n, &moved);
        }
    }
    for o in overrides {
        let Some(r) = rid_of(o) else { continue };
        // An override at its own instance's time (it changed something
        // else) moves with the series; one someone moved keeps its time.
        if let Some(start) = moment_of(o, "DTSTART").filter(|s| *s == r) {
            let end = end_of(o, &start);
            set_prop(o, "DTSTART", Some(start.shifted(shift).to_line("DTSTART")));
            remove_props(o, "DURATION");
            set_prop(o, "DTEND", Some(end.shifted(shift).to_line("DTEND")));
        }
        set_prop(o, "RECURRENCE-ID", Some(r.shifted(shift).to_line("RECURRENCE-ID")));
    }
}

fn edit_all(
    mut doc: Document,
    m: usize,
    events: &[usize],
    change: &EventChange,
    now: NaiveDateTime,
    mut zones: Vec<(String, i32)>,
) -> Result<Outcome, EditError> {
    let before: Vec<(&str, Option<String>)> = ["SUMMARY", "LOCATION", "DESCRIPTION"]
        .into_iter()
        .map(|n| (n, comp(&doc, m).property(n).map(|l| crate::object::unescape(l.value()))))
        .collect();
    let mut master = comp(&doc, m).clone();
    let applied = apply(&mut master, change, now, &mut zones)?;
    let others: Vec<usize> = events.iter().copied().filter(|&n| n != m).collect();
    let mut overrides: Vec<Component> = others.iter().map(|&n| comp(&doc, n).clone()).collect();
    let no_longer_repeats = matches!(change.rrule, Some(None));
    if no_longer_repeats {
        overrides.clear();
    } else {
        reattach(&mut master, &mut overrides, &applied);
        // A title, place or note an override shared with the master follows it.
        for o in &mut overrides {
            let mut follow = EventChange::default();
            for (name, old) in &before {
                let new = match *name {
                    "SUMMARY" => &change.summary,
                    "LOCATION" => &change.location,
                    _ => &change.description,
                };
                let theirs = o.property(name).map(|l| crate::object::unescape(l.value()));
                if new.is_some() && theirs == *old {
                    match *name {
                        "SUMMARY" => follow.summary = new.clone(),
                        "LOCATION" => follow.location = new.clone(),
                        _ => follow.description = new.clone(),
                    }
                }
            }
            if follow != EventChange::default() {
                apply(o, &follow, now, &mut zones)?;
            }
        }
    }
    *comp_mut(&mut doc, m) = master;
    for n in others.iter().rev() {
        doc.components[0].items.remove(*n);
    }
    let at = doc.components[0]
        .items
        .iter()
        .position(|i| matches!(i, Item::Component(c) if c.name == "VEVENT"))
        .map_or(0, |p| p + 1);
    for (k, o) in overrides.into_iter().enumerate() {
        doc.components[0].items.insert(at + k, Item::Component(o));
    }
    Ok(Outcome { updated: Some(finish(doc, &zones)), created: None })
}

/// Delete an event, or one occurrence (`This`), or an occurrence and the
/// rest of the series (`Following`).
pub fn delete(text: &str, instance: Option<Key>, scope: Scope, now: NaiveDateTime) -> Result<Outcome, EditError> {
    let gone = Outcome { updated: None, created: None };
    let Parsed { mut doc, events, master } = parse(text)?;
    let recurring = master.is_some_and(|m| is_recurring(comp(&doc, m)));
    let (Some(key), true, Some(m)) = (instance, recurring, master) else { return Ok(gone) };
    if scope == Scope::All {
        return Ok(gone);
    }
    let master_start = moment_of(comp(&doc, m), "DTSTART").ok_or(EditError::NoStart)?;
    if !instance_exists(text, &master_start, key, &doc) {
        return Err(EditError::NoSuchInstance);
    }
    let target = master_start.with_key(key);
    let at = instant(&target, &doc);
    let drop_override = |doc: &Document, n: usize, following: bool| {
        Some(n) != master
            && rid_of(comp(doc, n)).is_some_and(|r| {
                let i = instant(&r, doc);
                if following { i >= at } else { i == at }
            })
    };
    match scope {
        Scope::This => {
            let remove: Vec<usize> = events.iter().copied().filter(|&n| drop_override(&doc, n, false)).collect();
            for n in remove.iter().rev() {
                doc.components[0].items.remove(*n);
            }
            let mc = comp_mut(&mut doc, m);
            let mut list = date_list(mc, "EXDATE");
            list.push((String::new(), target));
            put_date_list(mc, "EXDATE", &list);
            restamp(mc, now);
        }
        Scope::Following => {
            if at <= instant(&master_start, &doc) {
                return Ok(gone);
            }
            let remove: Vec<usize> = events.iter().copied().filter(|&n| drop_override(&doc, n, true)).collect();
            for n in remove.iter().rev() {
                doc.components[0].items.remove(*n);
            }
            let snapshot = doc.clone();
            let mc = comp_mut(&mut doc, m);
            for name in ["EXDATE", "RDATE"] {
                let list = date_list(mc, name);
                if !list.is_empty() {
                    let early: Vec<_> = list.into_iter().filter(|(_, v)| instant(v, &snapshot) < at).collect();
                    put_date_list(mc, name, &early);
                }
            }
            end_series_before(mc, &target, &snapshot);
            restamp(mc, now);
        }
        Scope::All => unreachable!("handled above"),
    }
    Ok(Outcome { updated: Some(doc.to_string()), created: None })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moment::parse_naive;
    use chrono::NaiveDate;

    const NOW: &str = "20260110T120000";

    fn now() -> NaiveDateTime {
        parse_naive(NOW).unwrap()
    }

    fn t(s: &str) -> NaiveDateTime {
        parse_naive(s).unwrap()
    }

    fn ams(s: &str) -> Moment {
        Moment::Zoned { local: t(s), tzid: "Europe/Amsterdam".into() }
    }

    fn cal(body: &str) -> String {
        format!("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Test//EN\r\n{body}END:VCALENDAR\r\n")
    }

    /// Weekly on Mondays 09:00-10:00 Amsterdam, 10 times, from 5 Jan 2026.
    fn weekly(extra: &str) -> String {
        cal(&format!(
            "BEGIN:VEVENT\r\nUID:swim\r\nDTSTAMP:20260101T000000Z\r\nDTSTART;TZID=Europe/Amsterdam:20260105T090000\r\n\
             DTEND;TZID=Europe/Amsterdam:20260105T100000\r\nRRULE:FREQ=WEEKLY;COUNT=10\r\nSUMMARY:Swim\r\n\
             X-CUSTOM;FOO=bar:keep me\r\n{extra}END:VEVENT\r\n"
        ))
    }

    fn starts(text: &str) -> Vec<String> {
        let from = t("20250101T000000");
        let to = t("20280101T000000");
        recur::expand(text, from, to, chrono_tz::Europe::Amsterdam)
            .unwrap()
            .occurrences
            .iter()
            .map(|o| {
                use chrono::TimeZone;
                let utc = o.start.instant(chrono_tz::Europe::Amsterdam);
                chrono_tz::Europe::Amsterdam.from_utc_datetime(&utc).format("%m-%d %H:%M").to_string()
            })
            .collect()
    }

    fn lines(text: &str) -> Vec<String> {
        text.split("\r\n").map(str::to_string).collect()
    }

    #[test]
    fn untouched_lines_stay_byte_for_byte() {
        let src = weekly("ATTENDEE;CN=\"Doe, J\";PARTSTAT=ACCEPTED:mailto:j@example.org\r\n");
        let change = EventChange { summary: Some("Swimming".into()), ..Default::default() };
        let out = edit(&src, None, Scope::All, &change, now(), "x").unwrap().updated.unwrap();
        let (before, after) = (lines(&src), lines(&out));
        for keep in [
            "X-CUSTOM;FOO=bar:keep me",
            "ATTENDEE;CN=\"Doe, J\";PARTSTAT=ACCEPTED:mailto:j@example.org",
            "RRULE:FREQ=WEEKLY;COUNT=10",
            "DTSTART;TZID=Europe/Amsterdam:20260105T090000",
        ] {
            assert!(before.contains(&keep.to_string()) && after.contains(&keep.to_string()), "{keep}");
        }
        assert!(out.contains("SUMMARY:Swimming") && !out.contains("SUMMARY:Swim\r\n"));
        assert!(out.contains("LAST-MODIFIED:20260110T120000Z") && out.contains("DTSTAMP:20260110T120000Z"));
        assert!(!out.contains("SEQUENCE"), "a title change is not significant");
        // A no-op change touches only the stamps.
        let same = edit(&src, None, Scope::All, &EventChange::default(), now(), "x").unwrap().updated.unwrap();
        let diff: Vec<_> = lines(&same).into_iter().filter(|l| !lines(&src).contains(l)).collect();
        assert_eq!(diff, ["DTSTAMP:20260110T120000Z", "LAST-MODIFIED:20260110T120000Z"]);
    }

    #[test]
    fn moving_keeps_the_length_and_bumps_sequence() {
        let src = cal(
            "BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260105T090000Z\r\nDTEND:20260105T103000Z\r\nSEQUENCE:3\r\nSUMMARY:A\r\nEND:VEVENT\r\n",
        );
        let change = EventChange { start: Some(Moment::Utc(t("20260106T140000"))), ..Default::default() };
        let out = edit(&src, None, Scope::All, &change, now(), "x").unwrap().updated.unwrap();
        assert!(
            out.contains("DTSTART:20260106T140000Z")
                && out.contains("DTEND:20260106T153000Z")
                && out.contains("SEQUENCE:4")
        );
        // To all-day: one day by default.
        let day = EventChange {
            start: Some(Moment::Date(NaiveDate::from_ymd_opt(2026, 1, 7).unwrap())),
            ..Default::default()
        };
        let out = edit(&src, None, Scope::All, &day, now(), "x").unwrap().updated.unwrap();
        assert!(out.contains("DTSTART;VALUE=DATE:20260107") && out.contains("DTEND;VALUE=DATE:20260108"));
        let bad = EventChange { end: Some(Moment::Utc(t("20260105T080000"))), ..Default::default() };
        assert_eq!(edit(&src, None, Scope::All, &bad, now(), "x"), Err(EditError::EndBeforeStart));
    }

    #[test]
    fn durations_become_an_end_when_the_end_is_set() {
        let src =
            cal("BEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260105T090000Z\r\nDURATION:PT45M\r\nSUMMARY:A\r\nEND:VEVENT\r\n");
        let moved = EventChange { start: Some(Moment::Utc(t("20260105T100000"))), ..Default::default() };
        let out = edit(&src, None, Scope::All, &moved, now(), "x").unwrap().updated.unwrap();
        assert!(out.contains("DTEND:20260105T104500Z") && !out.contains("DURATION"), "{out}");
        assert_eq!(parse_duration("-P1DT2H"), Some(-(Duration::days(1) + Duration::hours(2))));
        assert_eq!(parse_duration("P2W"), Some(Duration::weeks(2)));
    }

    #[test]
    fn only_this_one_makes_an_override() {
        let src = weekly("");
        let key: Key = "20260112T090000".parse().unwrap();
        let change = EventChange {
            start: Some(ams("20260113T180000")),
            summary: Some("Swim (moved)".into()),
            ..Default::default()
        };
        let out = edit(&src, Some(key), Scope::This, &change, now(), "x").unwrap();
        let text = out.updated.unwrap();
        assert!(out.created.is_none());
        assert!(text.contains("RECURRENCE-ID;TZID=Europe/Amsterdam:20260112T090000"));
        assert!(
            text.contains("DTSTART;TZID=Europe/Amsterdam:20260113T180000")
                && text.contains("DTEND;TZID=Europe/Amsterdam:20260113T190000")
        );
        assert_eq!(text.matches("RRULE:FREQ=WEEKLY").count(), 1, "the override does not repeat");
        let s = starts(&text);
        assert_eq!(s.len(), 10);
        assert!(s.contains(&"01-13 18:00".into()) && !s.contains(&"01-12 09:00".into()));
        // Editing it again changes the same override.
        let again = EventChange { summary: Some("Swim (moved again)".into()), ..Default::default() };
        let text2 = edit(&text, Some(key), Scope::This, &again, now(), "x").unwrap().updated.unwrap();
        assert_eq!(text2.matches("RECURRENCE-ID").count(), 1);
        assert!(text2.contains("Swim (moved again)"));
        // Not an instance.
        let nope: Key = "20260112T100000".parse().unwrap();
        assert_eq!(edit(&src, Some(nope), Scope::This, &change, now(), "x"), Err(EditError::NoSuchInstance));
    }

    #[test]
    fn this_and_following_splits_the_series() {
        let src = weekly("EXDATE;TZID=Europe/Amsterdam:20260119T090000,20260223T090000\r\n");
        // An override in the later part.
        let src = edit(
            &src,
            Some("20260302T090000".parse().unwrap()),
            Scope::This,
            &EventChange { summary: Some("Gala".into()), ..Default::default() },
            now(),
            "x",
        )
        .unwrap()
        .updated
        .unwrap();
        let key: Key = "20260209T090000".parse().unwrap();
        let change = EventChange { start: Some(ams("20260209T100000")), ..Default::default() };
        let out = edit(&src, Some(key), Scope::Following, &change, now(), "swim-2").unwrap();
        let (old, new) = (out.updated.unwrap(), out.created.unwrap());
        // Old: up to 2 Feb, its early EXDATE kept, the count became an UNTIL.
        assert!(old.contains("UNTIL=20260209T075959Z") && !old.contains("COUNT"), "{old}");
        assert_eq!(starts(&old), ["01-05 09:00", "01-12 09:00", "01-26 09:00", "02-02 09:00"]);
        assert!(!old.contains("Gala") && !old.contains("20260223"));
        // New: 6 left of the 10 (5 Jan to 2 Feb were 5 instances), an hour later.
        assert!(new.contains("UID:swim-2") && new.contains("COUNT=5") && new.contains("X-CUSTOM;FOO=bar:keep me"));
        assert!(new.contains("BEGIN:VTIMEZONE"), "the new series writes a zoned time, so it carries its zone");
        let s = starts(&new);
        assert_eq!(s, ["02-09 10:00", "02-16 10:00", "03-02 10:00", "03-09 10:00"]);
        assert!(
            new.contains("RECURRENCE-ID;TZID=Europe/Amsterdam:20260302T100000"),
            "the override moved with the shift"
        );
        assert!(new.contains("EXDATE;TZID=Europe/Amsterdam:20260223T100000"));
        assert!(new.contains("SUMMARY:Gala"));
        // Together: every instance once, none lost, none doubled.
        assert_eq!(starts(&old).len() + starts(&new).len(), 8);
    }

    #[test]
    fn following_from_the_first_is_all() {
        let src = weekly("");
        let out = edit(
            &src,
            Some("20260105T090000".parse().unwrap()),
            Scope::Following,
            &EventChange { summary: Some("S".into()), ..Default::default() },
            now(),
            "n",
        )
        .unwrap();
        assert!(out.created.is_none());
        assert!(out.updated.unwrap().contains("COUNT=10"));
    }

    #[test]
    fn all_moves_overrides_and_exceptions_along() {
        let src = weekly("EXDATE;TZID=Europe/Amsterdam:20260119T090000\r\n");
        let src = edit(
            &src,
            Some("20260126T090000".parse().unwrap()),
            Scope::This,
            &EventChange { location: Some("Other pool".into()), ..Default::default() },
            now(),
            "x",
        )
        .unwrap()
        .updated
        .unwrap();
        // The 12 January occurrence moved to 07:30, "all of them".
        let change = EventChange {
            start: Some(ams("20260112T073000")),
            summary: Some("Early swim".into()),
            ..Default::default()
        };
        let out = edit(&src, Some("20260112T090000".parse().unwrap()), Scope::All, &change, now(), "x")
            .unwrap()
            .updated
            .unwrap();
        assert!(out.contains("EXDATE;TZID=Europe/Amsterdam:20260119T073000"));
        assert!(out.contains("RECURRENCE-ID;TZID=Europe/Amsterdam:20260126T073000"));
        assert_eq!(out.matches("SUMMARY:Early swim").count(), 2, "the override shared the title, so it follows");
        let s = starts(&out);
        assert_eq!(s.len(), 9);
        assert!(s.iter().all(|x| x.ends_with("07:30")), "the override was never moved, so it follows: {s:?}");
        // No longer repeating drops the overrides and exceptions.
        let single = edit(&out, None, Scope::All, &EventChange { rrule: Some(None), ..Default::default() }, now(), "x")
            .unwrap()
            .updated
            .unwrap();
        assert!(
            !single.contains("RRULE:FREQ=WEEKLY") && !single.contains("EXDATE") && !single.contains("RECURRENCE-ID")
        );
        assert_eq!(starts(&single).len(), 1);
    }

    #[test]
    fn deleting_one_the_rest_or_all() {
        let src = weekly("");
        let key: Key = "20260112T090000".parse().unwrap();
        let one = delete(&src, Some(key), Scope::This, now()).unwrap().updated.unwrap();
        assert!(one.contains("EXDATE;TZID=Europe/Amsterdam:20260112T090000") && one.contains("SEQUENCE:1"));
        assert_eq!(starts(&one).len(), 9);
        let rest =
            delete(&src, Some("20260202T090000".parse().unwrap()), Scope::Following, now()).unwrap().updated.unwrap();
        assert_eq!(starts(&rest).len(), 4);
        assert!(
            delete(&src, Some("20260105T090000".parse().unwrap()), Scope::Following, now()).unwrap().updated.is_none()
        );
        assert!(delete(&src, Some(key), Scope::All, now()).unwrap().updated.is_none());
        assert!(delete(&src, None, Scope::This, now()).unwrap().updated.is_none(), "a whole event");
        // Deleting a moved instance removes its override too.
        let moved = edit(
            &src,
            Some(key),
            Scope::This,
            &EventChange { summary: Some("M".into()), ..Default::default() },
            now(),
            "x",
        )
        .unwrap()
        .updated
        .unwrap();
        let gone = delete(&moved, Some(key), Scope::This, now()).unwrap().updated.unwrap();
        assert!(!gone.contains("RECURRENCE-ID") && starts(&gone).len() == 9);
    }

    #[test]
    fn new_events_and_reminders() {
        let change = EventChange {
            summary: Some("Dentist, check-up".into()),
            start: Some(ams("20260115T150000")),
            location: Some("Main st; 2".into()),
            alarms: Some(vec![30, 1440]),
            ..Default::default()
        };
        let text = new_event(&change, "new-1", now()).unwrap();
        let doc = Document::parse(&text).unwrap();
        let info = crate::object::inspect_calendar(&doc, &text).unwrap();
        assert_eq!(info.uid, "new-1");
        assert!(text.contains("SUMMARY:Dentist\\, check-up") && text.contains("LOCATION:Main st\\; 2"));
        assert!(text.contains("DTEND;TZID=Europe/Amsterdam:20260115T160000"));
        assert!(text.contains("BEGIN:VTIMEZONE") && text.contains("TZID:Europe/Amsterdam"));
        assert!(text.contains("TRIGGER:-PT30M") && text.contains("TRIGGER:-P1D"));
        // The reminders are the person's own: the split takes them out.
        let mut d = Document::parse(&text).unwrap();
        let layer = crate::split::split(&mut d);
        assert_eq!(layer.entries.len(), 2);
        assert!(!d.to_string().contains("VALARM"));
        // Replacing them.
        let fewer =
            edit(&text, None, Scope::All, &EventChange { alarms: Some(vec![10]), ..Default::default() }, now(), "x")
                .unwrap()
                .updated
                .unwrap();
        assert_eq!(fewer.matches("BEGIN:VALARM").count(), 1);
        assert!(fewer.contains("TRIGGER:-PT10M"));
        assert_eq!(new_event(&EventChange::default(), "x", now()), Err(EditError::Incomplete));
        let bad_rule = EventChange { rrule: Some(Some("WEEKLY".into())), ..change };
        assert!(matches!(new_event(&bad_rule, "x", now()), Err(EditError::BadRule(_))));
    }

    #[test]
    fn a_floating_all_day_series() {
        let src = cal(
            "BEGIN:VEVENT\r\nUID:b\r\nDTSTART;VALUE=DATE:20260301\r\nDTEND;VALUE=DATE:20260302\r\nRRULE:FREQ=YEARLY\r\nSUMMARY:Birthday\r\nEND:VEVENT\r\n",
        );
        let out = delete(&src, Some("20280301".parse().unwrap()), Scope::Following, now()).unwrap().updated.unwrap();
        assert!(out.contains("UNTIL=20280229"), "{out}");
        assert_eq!(starts(&out), ["03-01 00:00", "03-01 00:00"]);
        let split = edit(
            &src,
            Some("20270301".parse().unwrap()),
            Scope::Following,
            &EventChange { summary: Some("Party".into()), ..Default::default() },
            now(),
            "b2",
        )
        .unwrap();
        assert!(split.created.unwrap().contains("DTSTART;VALUE=DATE:20270301"));
    }

    #[test]
    fn all_from_one_occurrence_moves_the_series_by_the_same_amount() {
        let src = weekly("");
        // Drag the third Monday from 09:00 to 10:30, "all of them".
        let change = EventChange {
            start: Some(ams("20260119T103000")),
            end: Some(ams("20260119T113000")),
            ..Default::default()
        };
        let out = edit(&src, Some("20260119T090000".parse().unwrap()), Scope::All, &change, now(), "x")
            .unwrap()
            .updated
            .unwrap();
        assert!(
            out.contains("DTSTART;TZID=Europe/Amsterdam:20260105T103000"),
            "the series starts where it did, 90 minutes later: {out}"
        );
        let s = starts(&out);
        assert_eq!(s.len(), 10);
        assert!(s.iter().all(|x| x.ends_with("10:30")), "{s:?}");
        // To the day before, all day: every occurrence becomes the Sunday.
        let day = EventChange {
            start: Some(Moment::Date(NaiveDate::from_ymd_opt(2026, 1, 18).unwrap())),
            ..Default::default()
        };
        let out = edit(&src, Some("20260119T090000".parse().unwrap()), Scope::All, &day, now(), "x")
            .unwrap()
            .updated
            .unwrap();
        assert!(out.contains("DTSTART;VALUE=DATE:20260104"), "{out}");
    }
}
