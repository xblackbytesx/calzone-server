//! The calendar as a person sees it, shared by the web app and the phone's
//! own screens (phase 4b): occurrences in the viewer's wall-clock time, one
//! event as the editor shows it, and the editor's input turned into the
//! core's [`EventChange`]. Times come in and go out as the viewer's own wall
//! time; each event keeps its own kind (floating stays floating, a zoned
//! event keeps its zone), as decision 37 settled for the web.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};

use crate::edit::{self, EventChange};
use crate::lines::{Component, Document};
use crate::moment::{Key, Moment, zoned_to_utc};
use crate::object::unescape;
use crate::recur::{self, When};

/// The editor's reminder presets, in minutes before the start.
pub const ALARM_PRESETS: [i64; 6] = [0, 5, 15, 30, 60, 1440];

/// The color of a calendar that has none.
pub const DEFAULT_COLOR: &str = "#3a7bd5";

/// A time from the editor: a date, or a wall-clock time in the viewer's zone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeInput {
    Date(NaiveDate),
    Local(NaiveDateTime),
}

impl std::str::FromStr for TimeInput {
    type Err = ();
    /// `2026-01-05`, `2026-01-05T09:00` or `2026-01-05T09:00:00`.
    fn from_str(s: &str) -> Result<Self, ()> {
        if let Ok(d) = NaiveDate::parse_from_str(s, "%Y-%m-%d") {
            return Ok(TimeInput::Date(d));
        }
        NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S")
            .or_else(|_| NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M"))
            .map(TimeInput::Local)
            .map_err(|_| ())
    }
}

fn viewer_to_utc(viewer: chrono_tz::Tz, local: NaiveDateTime) -> NaiveDateTime {
    viewer.from_local_datetime(&local).earliest().map(|t| t.naive_utc()).unwrap_or(local)
}

/// A viewer-local time as a moment of the same kind as the event's own:
/// floating stays floating, UTC stays UTC, a zoned event keeps its zone
/// (converted, so 10:00 for the viewer is 10:00 for the viewer). New
/// events and all-day events turned timed take the viewer's zone.
pub fn to_moment(input: TimeInput, like: Option<&Moment>, viewer: chrono_tz::Tz) -> Moment {
    let local = match input {
        TimeInput::Date(d) => return Moment::Date(d),
        TimeInput::Local(l) => l,
    };
    match like {
        Some(Moment::Floating(_)) => Moment::Floating(local),
        Some(Moment::Utc(_)) => Moment::Utc(viewer_to_utc(viewer, local)),
        Some(Moment::Zoned { tzid, .. }) => match tzid.parse::<chrono_tz::Tz>() {
            Ok(tz) => Moment::Zoned {
                local: tz.from_utc_datetime(&viewer_to_utc(viewer, local)).naive_local(),
                tzid: tzid.clone(),
            },
            Err(_) => Moment::Zoned { local, tzid: viewer.name().to_string() },
        },
        _ => Moment::Zoned { local, tzid: viewer.name().to_string() },
    }
}

/// How often a simple repeat happens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Freq {
    Daily,
    Weekly,
    Monthly,
    Yearly,
}

impl Freq {
    fn rule(self) -> &'static str {
        match self {
            Freq::Daily => "DAILY",
            Freq::Weekly => "WEEKLY",
            Freq::Monthly => "MONTHLY",
            Freq::Yearly => "YEARLY",
        }
    }

    fn from_rule(v: &str) -> Option<Freq> {
        Some(match v {
            "DAILY" => Freq::Daily,
            "WEEKLY" => Freq::Weekly,
            "MONTHLY" => Freq::Monthly,
            "YEARLY" => Freq::Yearly,
            _ => return None,
        })
    }

    pub fn name(self) -> &'static str {
        match self {
            Freq::Daily => "daily",
            Freq::Weekly => "weekly",
            Freq::Monthly => "monthly",
            Freq::Yearly => "yearly",
        }
    }
}

/// A repeat the editor offers: every day, week, month or year, forever,
/// until a day (the last day an occurrence may start, inclusive), or a
/// number of times.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repeat {
    pub freq: Freq,
    /// `2026-07-01`.
    #[serde(default, skip_serializing_if = "Option::is_none", with = "day")]
    pub until: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count: Option<u32>,
}

/// An optional day as `2026-07-01` (the core's chrono has no serde).
mod day {
    use chrono::NaiveDate;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(d: &Option<NaiveDate>, s: S) -> Result<S::Ok, S::Error> {
        match d {
            Some(d) => s.serialize_str(&d.format("%Y-%m-%d").to_string()),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<NaiveDate>, D::Error> {
        match Option::<String>::deserialize(d)? {
            None => Ok(None),
            Some(v) => NaiveDate::parse_from_str(v.trim(), "%Y-%m-%d").map(Some).map_err(serde::de::Error::custom),
        }
    }
}

impl Repeat {
    /// The RRULE value for a series starting at `start`. `UNTIL` takes the
    /// start's own form (RFC 5545 3.3.10) at the start's time on that day,
    /// so the last day keeps its occurrence.
    pub fn rule(&self, start: &Moment, doc: &Document) -> String {
        let mut r = format!("FREQ={}", self.freq.rule());
        if let Some(day) = self.until {
            let last = match start.key() {
                Key::Date(_) => Key::Date(day),
                Key::Time(t) => Key::Time(day.and_time(t.time())),
            };
            r.push_str(";UNTIL=");
            r.push_str(&start.with_key(last).until_value(doc));
        } else if let Some(n) = self.count.filter(|n| *n > 0) {
            r.push_str(&format!(";COUNT={n}"));
        }
        r
    }
}

/// What the editor shows for a rule: one of its simple repeats (with the
/// end in the viewer's days), or `custom` for anything else, which is kept
/// as it is unless changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RepeatView {
    /// `none`, `daily`, `weekly`, `monthly`, `yearly` or `custom`.
    pub kind: &'static str,
    pub until: Option<NaiveDate>,
    pub count: Option<u32>,
}

/// Read a rule as the editor shows it. `INTERVAL=1` and `WKST` change
/// nothing for these repeats; any other part makes it `custom`.
pub fn repeat_of(rule: Option<&str>, viewer: chrono_tz::Tz) -> RepeatView {
    let custom = RepeatView { kind: "custom", until: None, count: None };
    let Some(rule) = rule else { return RepeatView { kind: "none", until: None, count: None } };
    let mut freq = None;
    let mut until = None;
    let mut count = None;
    for part in rule.trim().trim_start_matches("RRULE:").split(';').filter(|p| !p.is_empty()) {
        let Some((k, v)) = part.split_once('=') else { return custom };
        let v = v.trim().to_ascii_uppercase();
        match k.trim().to_ascii_uppercase().as_str() {
            "FREQ" => freq = Freq::from_rule(&v),
            "INTERVAL" if v == "1" => {}
            "WKST" => {}
            "COUNT" => match v.parse::<u32>() {
                Ok(n) if n > 0 => count = Some(n),
                _ => return custom,
            },
            "UNTIL" => match until_day(&v, viewer) {
                Some(d) => until = Some(d),
                None => return custom,
            },
            _ => return custom,
        }
    }
    match freq {
        Some(f) if !(until.is_some() && count.is_some()) => RepeatView { kind: f.name(), until, count },
        _ => custom,
    }
}

/// An `UNTIL` value as the viewer's day.
fn until_day(v: &str, viewer: chrono_tz::Tz) -> Option<NaiveDate> {
    if v.len() == 8 {
        return crate::moment::parse_date(v);
    }
    if let Some(u) = v.strip_suffix('Z') {
        let t = crate::moment::parse_naive(u)?;
        return Some(viewer.from_utc_datetime(&t).naive_local().date());
    }
    crate::moment::parse_naive(v).map(|t| t.date())
}

/// What the editor asks to change, times in the viewer's terms. `None`
/// leaves a field as it is.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EventInput {
    pub summary: Option<String>,
    pub location: Option<String>,
    pub description: Option<String>,
    pub start: Option<TimeInput>,
    pub end: Option<TimeInput>,
    /// A rule as it is (`Some(None)`: no longer repeats).
    pub rrule: Option<Option<String>>,
    /// One of the editor's repeats (`Some(None)`: no longer repeats); wins
    /// over `rrule`.
    pub repeat: Option<Option<Repeat>>,
    pub alarms: Option<Vec<i64>>,
}

impl EventInput {
    /// Only the person's own reminders change: their layer, for every
    /// occurrence, also on a calendar they can only view.
    pub fn reminders_only(&self) -> Option<&[i64]> {
        let other = self.summary.is_some()
            || self.location.is_some()
            || self.description.is_some()
            || self.start.is_some()
            || self.end.is_some()
            || self.rrule.is_some()
            || self.repeat.is_some();
        if other { None } else { self.alarms.as_deref() }
    }
}

/// The events of an object: the master first when there is one.
fn events(cal: &Component) -> Vec<&Component> {
    cal.components().filter(|c| c.name == "VEVENT").collect()
}

fn master<'a>(events: &[&'a Component]) -> Option<&'a Component> {
    events.iter().copied().find(|x| x.property("RECURRENCE-ID").is_none()).or(events.first().copied())
}

fn override_for<'a>(events: &[&'a Component], k: Key) -> Option<&'a Component> {
    events
        .iter()
        .copied()
        .find(|x| x.property("RECURRENCE-ID").and_then(Moment::from_line).is_some_and(|r| r.key() == k))
}

/// The start and end the instance (or else the master) states.
fn start_end_of(doc: &Document, instance: Option<Key>) -> (Option<Moment>, Option<Moment>) {
    let Some(cal) = doc.components.first() else { return (None, None) };
    let evs = events(cal);
    let pick = instance.and_then(|k| override_for(&evs, k)).or_else(|| master(&evs));
    let Some(c) = pick else { return (None, None) };
    (c.property("DTSTART").and_then(Moment::from_line), c.property("DTEND").and_then(Moment::from_line))
}

/// The editor's input as a change to `text` (`None` for a new event), at
/// `instance` when one occurrence is edited.
pub fn change_for(text: Option<&str>, instance: Option<Key>, input: &EventInput, viewer: chrono_tz::Tz) -> EventChange {
    let doc = text
        .and_then(|t| Document::parse(t).ok())
        .unwrap_or(Document { components: Vec::new(), trailing: String::new() });
    let (like_start, like_end) = start_end_of(&doc, instance);
    let start = input.start.map(|t| to_moment(t, like_start.as_ref(), viewer));
    let end = input.end.map(|t| to_moment(t, like_end.as_ref().or(like_start.as_ref()), viewer));
    let rrule = match input.repeat {
        Some(Some(r)) => {
            // The series' start after this change: the new one, else the master's.
            let series_start = start.clone().or_else(|| {
                let cal = doc.components.first()?;
                master(&events(cal))?.property("DTSTART").and_then(Moment::from_line)
            });
            // Without a start there is no event to repeat; the edit refuses it.
            series_start.map(|s| Some(r.rule(&s, &doc)))
        }
        Some(None) => Some(None),
        None => input.rrule.clone(),
    };
    EventChange {
        summary: input.summary.clone(),
        location: input.location.clone(),
        description: input.description.clone(),
        start,
        end,
        rrule,
        alarms: input.alarms.clone(),
    }
}

/// The instance key of an occurrence's start.
pub fn key_of(w: &When) -> Key {
    match *w {
        When::Date(d) => Key::Date(d),
        When::Utc(t) | When::Floating(t) => Key::Time(t),
        When::Zoned { local, .. } => Key::Time(local),
    }
}

/// A `When` as viewer-local text: `2026-01-05T09:00:00`, or `2026-01-05`.
pub fn local_text(w: &When, viewer: chrono_tz::Tz) -> String {
    match w {
        When::Date(d) => d.format("%Y-%m-%d").to_string(),
        other => viewer.from_utc_datetime(&other.instant(viewer)).naive_local().format("%Y-%m-%dT%H:%M:%S").to_string(),
    }
}

/// When one of my reminders rings, for an occurrence (phase 4b step 5).
/// The phone's store holds only my own VALARMs (decision 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// Seconds after the start (negative: before it), the usual kind.
    Start(i64),
    /// Seconds after the end (`RELATED=END`).
    End(i64),
    /// A fixed instant, UTC seconds (`VALUE=DATE-TIME`).
    At(i64),
}

impl Trigger {
    /// The instant it rings for an occurrence from `at` to `end_at`.
    pub fn fire(self, at: i64, end_at: i64) -> i64 {
        match self {
            Trigger::Start(s) => at + s,
            Trigger::End(s) => end_at + s,
            Trigger::At(t) => t,
        }
    }
}

/// The reminders of an event's parts: the master's, and each overridden
/// occurrence's by its instance key (an override has its own VALARMs, or
/// none).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Reminders {
    pub master: Vec<Trigger>,
    pub overrides: Vec<(String, Vec<Trigger>)>,
}

impl Reminders {
    /// The reminders of one occurrence (`instance` as [`Occurrence::instance`]).
    pub fn of(&self, instance: Option<&str>) -> &[Trigger] {
        instance
            .and_then(|k| self.overrides.iter().find(|(o, _)| o == k))
            .map(|(_, t)| t.as_slice())
            .unwrap_or(&self.master)
    }

    pub fn is_empty(&self) -> bool {
        self.master.is_empty() && self.overrides.iter().all(|(_, t)| t.is_empty())
    }
}

fn triggers(c: &Component) -> Vec<Trigger> {
    c.components()
        .filter(|a| a.name == "VALARM")
        .filter_map(|a| a.property("TRIGGER"))
        .filter_map(|t| {
            if t.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE-TIME")) {
                let v = t.value().trim().trim_end_matches('Z');
                return crate::moment::parse_naive(v).map(|u| Trigger::At(u.and_utc().timestamp()));
            }
            let d = edit::parse_duration(t.value())?.num_seconds();
            Some(if t.param("RELATED").is_some_and(|r| r.eq_ignore_ascii_case("END")) {
                Trigger::End(d)
            } else {
                Trigger::Start(d)
            })
        })
        .collect()
}

/// The reminders in an event's text; empty when it has none or is not an event.
pub fn reminders_of(text: &str) -> Reminders {
    // Most events have none: skip parsing them.
    if !text.contains("VALARM") {
        return Reminders::default();
    }
    let Ok(doc) = Document::parse(text) else { return Reminders::default() };
    let Some(cal) = doc.components.first() else { return Reminders::default() };
    let evs = events(cal);
    let Some(m) = master(&evs) else { return Reminders::default() };
    let overrides = evs
        .iter()
        .filter(|e| !std::ptr::eq(**e, m))
        .filter_map(|e| {
            e.property("RECURRENCE-ID").and_then(Moment::from_line).map(|r| (r.key().to_string(), triggers(e)))
        })
        .collect();
    Reminders { master: triggers(m), overrides }
}

/// Whether an event's title, place or notes hold every one of `words`
/// (lowercase), in the master or in one of its overridden occurrences.
pub fn matches(text: &str, words: &[String]) -> bool {
    if words.is_empty() {
        return false;
    }
    let Ok(doc) = Document::parse(text) else { return false };
    let Some(cal) = doc.components.first() else { return false };
    events(cal).iter().any(|e| {
        let hay = ["SUMMARY", "LOCATION", "DESCRIPTION"]
            .iter()
            .filter_map(|n| e.property(n).map(|l| unescape(l.value())))
            .collect::<Vec<_>>()
            .join("\n")
            .to_lowercase();
        words.iter().all(|w| hay.contains(w.as_str()))
    })
}

/// One occurrence of an event, in the viewer's time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Occurrence {
    /// The instance key of a recurring event (`20260105T090000`).
    pub instance: Option<String>,
    pub title: String,
    pub location: Option<String>,
    /// Viewer-local `2026-01-05T09:00:00`, or `2026-01-05` when all day
    /// (the end is then the day after the last day).
    pub start: String,
    pub end: String,
    pub all_day: bool,
    pub recurring: bool,
    /// The instants it starts and ends (UTC seconds), for sorting,
    /// reminders and ranges.
    pub at: i64,
    pub end_at: i64,
}

/// The occurrences of one object overlapping `[from, to)` (UTC), in the
/// viewer's zone. Not an event (a task, a journal): none.
pub fn occurrences(text: &str, from: NaiveDateTime, to: NaiveDateTime, viewer: chrono_tz::Tz) -> Vec<Occurrence> {
    let Ok(exp) = recur::expand(text, from, to, viewer) else { return Vec::new() };
    let doc = Document::parse(text).ok();
    let first =
        doc.as_ref().and_then(|d| d.components.first().and_then(|c| c.components().find(|x| x.name == "VEVENT")));
    let Some(first) = first else { return Vec::new() };
    // A birthday or anniversary (decision 57): the age, per year shown.
    let since: Option<i32> = first.property(crate::birthdays::SINCE).and_then(|l| l.value().trim().parse().ok());
    exp.occurrences
        .into_iter()
        .map(|o| {
            let instance = o.recurrence_id.as_ref().map(|r| key_of(r).to_string());
            Occurrence {
                recurring: instance.is_some(),
                instance,
                title: {
                    let t = o.summary.clone().unwrap_or_default();
                    let year = match &o.start {
                        When::Date(d) => d.year(),
                        w => w.instant(viewer).year(),
                    };
                    match crate::birthdays::age(since, year) {
                        Some(a) => format!("{t} ({a})"),
                        None => t,
                    }
                },
                location: o.location.clone(),
                start: local_text(&o.start, viewer),
                end: local_text(&o.end, viewer),
                all_day: matches!(o.start, When::Date(_)),
                at: o.start.instant(viewer).and_utc().timestamp(),
                end_at: o.end.instant(viewer).and_utc().timestamp().max(o.start.instant(viewer).and_utc().timestamp()),
            }
        })
        .collect()
}

/// One event (or occurrence) as the editor shows it, in the viewer's own
/// wall-clock time, with the viewer's own reminders.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Detail {
    pub instance: Option<String>,
    pub title: String,
    pub location: String,
    pub description: String,
    pub all_day: bool,
    /// `2026-01-05`; the time `09:00`, empty when all day.
    pub start_date: String,
    pub start_time: String,
    /// For all-day events the last day (inclusive), as people say it.
    pub end_date: String,
    pub end_time: String,
    /// `none`, `daily`, `weekly`, `monthly`, `yearly`, or `custom` for any
    /// other rule (kept as it is unless changed); with its end.
    pub repeat: &'static str,
    pub repeat_until: Option<String>,
    pub repeat_count: Option<u32>,
    pub recurring: bool,
    /// Every reminder, minutes before the start.
    pub alarms: Vec<i64>,
    /// The one the editor shows: `None` with `alarm_kept` when it is not
    /// one the editor offers (kept as it is unless changed).
    pub alarm: Option<i64>,
    pub alarm_kept: bool,
}

/// A moment as viewer-local (date, time; no time when all day).
fn local_parts(m: &Moment, doc: &Document, viewer: chrono_tz::Tz) -> (NaiveDate, Option<NaiveTime>) {
    let utc_to_local = |u: NaiveDateTime| viewer.from_utc_datetime(&u).naive_local();
    let local = match m {
        Moment::Date(d) => return (*d, None),
        Moment::Floating(t) => *t,
        Moment::Utc(t) => utc_to_local(*t),
        Moment::Zoned { local, tzid } => utc_to_local(zoned_to_utc(*local, tzid, doc)),
    };
    (local.date(), Some(local.time()))
}

/// The event (`instance`: one occurrence) as the editor shows it; `None`
/// when it is not an event.
pub fn detail(text: &str, instance: Option<Key>, viewer: chrono_tz::Tz) -> Option<Detail> {
    let doc = Document::parse(text).ok()?;
    let cal = doc.components.first()?;
    let evs = events(cal);
    let master = master(&evs)?;
    let master_start = master.property("DTSTART").and_then(Moment::from_line)?;
    let master_end = master.property("DTEND").and_then(Moment::from_line).unwrap_or_else(|| {
        match master.property("DURATION").and_then(|l| edit::parse_duration(l.value())) {
            Some(d) => master_start.shifted(d),
            None if master_start.is_date() => master_start.shifted(Duration::days(1)),
            None => master_start.clone(),
        }
    });
    let recurring = master.property("RRULE").is_some() || master.property("RDATE").is_some();
    let over = instance.and_then(|k| override_for(&evs, k));
    let part = over.unwrap_or(master);
    let (start, end) = match (over, instance) {
        (Some(o), _) => {
            let s = o.property("DTSTART").and_then(Moment::from_line).unwrap_or_else(|| master_start.clone());
            let e = o.property("DTEND").and_then(Moment::from_line).unwrap_or_else(|| s.clone());
            (s, e)
        }
        (None, Some(k)) if recurring => {
            let s = master_start.with_key(k);
            let len = match (master_start.key(), master_end.key()) {
                (Key::Date(a), Key::Date(b)) => b - a,
                (Key::Time(a), Key::Time(b)) => b - a,
                _ => Duration::zero(),
            };
            (s.clone(), s.shifted(len))
        }
        _ => (master_start.clone(), master_end.clone()),
    };
    let (sd, st) = local_parts(&start, &doc, viewer);
    let (mut ed, et) = local_parts(&end, &doc, viewer);
    let all_day = st.is_none();
    if all_day {
        // Stored exclusive; shown as the last day.
        ed = (ed - Duration::days(1)).max(sd);
    }
    let text = |n: &str| part.property(n).map(|l| unescape(l.value())).unwrap_or_default();
    let alarms: Vec<i64> = part
        .components()
        .filter(|a| a.name == "VALARM")
        .filter_map(|a| a.property("TRIGGER"))
        .filter(|t| t.param("VALUE").is_none_or(|v| !v.eq_ignore_ascii_case("DATE-TIME")))
        .filter_map(|t| edit::parse_duration(t.value()))
        .map(|d| -d.num_minutes())
        .collect();
    let (alarm, alarm_kept) = match alarms.as_slice() {
        [] => (None, false),
        [m] if ALARM_PRESETS.contains(m) => (Some(*m), false),
        _ => (None, true),
    };
    let repeat = repeat_of(master.property("RRULE").map(|l| l.value()), viewer);
    let fmt_t = |t: Option<NaiveTime>| t.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
    Some(Detail {
        instance: instance.filter(|_| recurring || over.is_some()).map(|k| k.to_string()),
        title: text("SUMMARY"),
        location: text("LOCATION"),
        description: text("DESCRIPTION"),
        all_day,
        start_date: sd.format("%Y-%m-%d").to_string(),
        start_time: fmt_t(st),
        end_date: ed.format("%Y-%m-%d").to_string(),
        end_time: fmt_t(et),
        repeat: repeat.kind,
        repeat_until: repeat.until.map(|d| d.format("%Y-%m-%d").to_string()),
        repeat_count: repeat.count,
        recurring,
        alarms,
        alarm,
        alarm_kept,
    })
}

/// Quick add's draft as the editor takes it: the title, and viewer-local
/// times like [`Occurrence`]'s (`None` when no day or time was found, so
/// the editor opens with the title).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QuickDraft {
    pub title: String,
    pub all_day: bool,
    pub start: Option<String>,
    pub end: Option<String>,
}

/// "dentist thu 3pm" read in `lang`, with `local_now` the viewer's wall
/// clock. `None`: nothing to make a title of.
pub fn quick_draft(text: &str, lang: crate::quickadd::Lang, local_now: NaiveDateTime) -> Option<QuickDraft> {
    use crate::quickadd::QuickWhen;
    let d = crate::quickadd::parse(text, lang, local_now)?;
    let (all_day, start, end) = match d.when {
        None => (false, None, None),
        Some(QuickWhen::Days { start, end }) => {
            (true, Some(start.format("%Y-%m-%d").to_string()), Some(end.format("%Y-%m-%d").to_string()))
        }
        Some(QuickWhen::Time { start, end }) => (
            false,
            Some(start.format("%Y-%m-%dT%H:%M:%S").to_string()),
            Some(end.format("%Y-%m-%dT%H:%M:%S").to_string()),
        ),
    };
    Some(QuickDraft { title: d.title, all_day, start, end })
}

/// Initials of a name, for events on calendars the viewer does not own and
/// for contacts: the first and last word ("Bram de Vries" is BV), the first
/// two letters of a single word, `?` for none. The web's script does the same.
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let pick = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [first, .., last] => pick(first) + &pick(last),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    const AMS: chrono_tz::Tz = chrono_tz::Europe::Amsterdam;

    #[test]
    fn reminder_triggers_of_each_kind() {
        let text = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTAMP:20260101T000000Z\r\n\
            DTSTART:20260107T150000Z\r\nDTEND:20260107T160000Z\r\nSUMMARY:A\r\n\
            BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-P1D\r\nEND:VALARM\r\n\
            BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;RELATED=END:-PT5M\r\nEND:VALARM\r\n\
            BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER;VALUE=DATE-TIME:20260106T080000Z\r\nEND:VALARM\r\n\
            END:VEVENT\r\nEND:VCALENDAR\r\n";
        let r = reminders_of(text);
        let at = NaiveDate::from_ymd_opt(2026, 1, 6).unwrap().and_hms_opt(8, 0, 0).unwrap().and_utc().timestamp();
        assert_eq!(r.master, vec![Trigger::Start(-86_400), Trigger::End(-300), Trigger::At(at)]);
        assert_eq!(Trigger::End(-300).fire(1000, 4600), 4300);
        assert_eq!(r.of(Some("20260107T150000")), r.master.as_slice(), "no override: the master's");
    }

    fn empty() -> Document {
        Document { components: Vec::new(), trailing: String::new() }
    }

    #[test]
    fn editor_times() {
        assert_eq!("2026-01-05".parse(), Ok(TimeInput::Date(d("2026-01-05"))));
        assert_eq!("2026-01-05T09:30".parse(), Ok(TimeInput::Local(t("2026-01-05 09:30"))));
        assert_eq!("2026-01-05T09:30:00".parse(), Ok(TimeInput::Local(t("2026-01-05 09:30"))));
        assert!("monday".parse::<TimeInput>().is_err());
    }

    #[test]
    fn times_keep_the_events_kind() {
        let local = TimeInput::Local(t("2026-01-05 10:00"));
        assert_eq!(
            to_moment(local, Some(&Moment::Floating(t("2026-01-01 00:00"))), AMS),
            Moment::Floating(t("2026-01-05 10:00"))
        );
        assert_eq!(
            to_moment(local, Some(&Moment::Utc(t("2026-01-01 00:00"))), AMS),
            Moment::Utc(t("2026-01-05 09:00"))
        );
        let ny = Moment::Zoned { local: t("2026-01-01 00:00"), tzid: "America/New_York".into() };
        assert_eq!(
            to_moment(local, Some(&ny), AMS),
            Moment::Zoned { local: t("2026-01-05 04:00"), tzid: "America/New_York".into() }
        );
        assert_eq!(
            to_moment(local, None, AMS),
            Moment::Zoned { local: t("2026-01-05 10:00"), tzid: "Europe/Amsterdam".into() }
        );
        assert!(to_moment(TimeInput::Date(d("2026-01-05")), Some(&ny), AMS).is_date());
    }

    #[test]
    fn repeats_with_an_end() {
        let zoned = Moment::Zoned { local: t("2026-01-07 16:00"), tzid: "Europe/Amsterdam".into() };
        let until = Repeat { freq: Freq::Weekly, until: Some(d("2026-07-01")), count: None };
        // 16:00 in Amsterdam on the last day is 14:00 UTC in summer.
        assert_eq!(until.rule(&zoned, &empty()), "FREQ=WEEKLY;UNTIL=20260701T140000Z");
        assert_eq!(until.rule(&Moment::Date(d("2026-01-07")), &empty()), "FREQ=WEEKLY;UNTIL=20260701");
        assert_eq!(until.rule(&Moment::Floating(t("2026-01-07 16:00")), &empty()), "FREQ=WEEKLY;UNTIL=20260701T160000");
        let count = Repeat { freq: Freq::Daily, until: None, count: Some(5) };
        assert_eq!(count.rule(&zoned, &empty()), "FREQ=DAILY;COUNT=5");
        let forever = Repeat { freq: Freq::Yearly, until: None, count: None };
        assert_eq!(forever.rule(&zoned, &empty()), "FREQ=YEARLY");

        // And read back as the editor shows them.
        let read = |r: &str| repeat_of(Some(r), AMS);
        assert_eq!(read("FREQ=WEEKLY;UNTIL=20260701T140000Z").until, Some(d("2026-07-01")));
        assert_eq!(read("FREQ=WEEKLY;UNTIL=20260701T140000Z").kind, "weekly");
        assert_eq!(read("FREQ=DAILY;COUNT=5").count, Some(5));
        assert_eq!(read("FREQ=WEEKLY;INTERVAL=1;WKST=MO").kind, "weekly");
        assert_eq!(read("FREQ=WEEKLY;BYDAY=MO,WE").kind, "custom");
        assert_eq!(read("FREQ=WEEKLY;INTERVAL=2").kind, "custom");
        assert_eq!(read("FREQ=HOURLY").kind, "custom");
        assert_eq!(repeat_of(None, AMS).kind, "none");
    }

    const SWIM: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:s\r\nDTSTAMP:20260101T000000Z\r\n\
        DTSTART;TZID=Europe/Amsterdam:20260107T160000\r\nDTEND;TZID=Europe/Amsterdam:20260107T170000\r\n\
        SUMMARY:Swim\r\nRRULE:FREQ=WEEKLY\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:x\r\n\
        TRIGGER:-PT15M\r\nEND:VALARM\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn an_end_on_an_existing_series() {
        let input = EventInput {
            repeat: Some(Some(Repeat { freq: Freq::Weekly, until: Some(d("2026-07-01")), count: None })),
            ..EventInput::default()
        };
        let change = change_for(Some(SWIM), None, &input, AMS);
        assert_eq!(change.rrule, Some(Some("FREQ=WEEKLY;UNTIL=20260701T140000Z".into())));
        let out = edit::edit(SWIM, None, edit::Scope::All, &change, t("2026-01-02 00:00"), "n").unwrap();
        let text = out.updated.unwrap();
        let shown = detail(&text, None, AMS).unwrap();
        assert_eq!((shown.repeat, shown.repeat_until.as_deref()), ("weekly", Some("2026-07-01")));
        // The last Wednesday keeps its lesson; there is none after.
        let occ = occurrences(&text, t("2026-06-01 00:00"), t("2026-12-31 00:00"), AMS);
        assert_eq!(occ.last().map(|o| o.start.as_str()), Some("2026-07-01T16:00:00"));

        let none = EventInput { repeat: Some(None), ..EventInput::default() };
        assert_eq!(change_for(Some(SWIM), None, &none, AMS).rrule, Some(None));
        let new = EventInput {
            summary: Some("Swim".into()),
            start: Some(TimeInput::Local(t("2026-01-07 16:00"))),
            repeat: Some(Some(Repeat { freq: Freq::Weekly, until: None, count: Some(10) })),
            ..EventInput::default()
        };
        assert_eq!(change_for(None, None, &new, AMS).rrule, Some(Some("FREQ=WEEKLY;COUNT=10".into())));
    }

    #[test]
    fn occurrences_and_detail_in_the_viewers_time() {
        let occ = occurrences(SWIM, t("2026-01-01 00:00"), t("2026-01-22 00:00"), AMS);
        assert_eq!(occ.len(), 3);
        assert_eq!(occ[0].instance.as_deref(), Some("20260107T160000"));
        assert_eq!(occ[0].start, "2026-01-07T16:00:00");
        assert_eq!(occ[0].at, t("2026-01-07 15:00").and_utc().timestamp());
        let london = occurrences(SWIM, t("2026-01-01 00:00"), t("2026-01-08 00:00"), chrono_tz::Europe::London);
        assert_eq!(london[0].start, "2026-01-07T15:00:00");

        let k: Key = "20260114T160000".parse().unwrap();
        let one = detail(SWIM, Some(k), AMS).unwrap();
        assert_eq!((one.start_date.as_str(), one.start_time.as_str()), ("2026-01-14", "16:00"));
        assert_eq!((one.end_time.as_str(), one.repeat), ("17:00", "weekly"));
        assert_eq!((one.alarm, one.alarms.clone()), (Some(15), vec![15]));
        assert!(one.recurring && one.instance.is_some());

        let input = EventInput { alarms: Some(vec![60]), ..EventInput::default() };
        assert_eq!(input.reminders_only(), Some(&[60][..]));
        let more = EventInput { summary: Some("x".into()), ..input };
        assert_eq!(more.reminders_only(), None);

        let todo = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:t\r\nDTSTAMP:20260101T000000Z\r\n\
            DTSTART:20260107T160000Z\r\nSUMMARY:x\r\nEND:VTODO\r\nEND:VCALENDAR\r\n";
        assert!(occurrences(todo, t("2026-01-01 00:00"), t("2026-02-01 00:00"), AMS).is_empty());
    }

    #[test]
    fn initials_of_names() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("Bram de Vries"), "BV");
        assert_eq!(initials("Ada Augusta King-Noel"), "AN");
        assert_eq!(initials("Pizzeria"), "PI");
        assert_eq!(initials("!!"), "?");
    }
}
