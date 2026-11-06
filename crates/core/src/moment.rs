//! Times as event properties write them (RFC 5545 3.3.4 and 3.3.5), for
//! editing: read a `DTSTART`-like line into a [`Moment`], write one back,
//! convert to UTC for `UNTIL`, and make a VTIMEZONE for a zone an edit
//! introduces (RFC 5545 asks for one per TZID; Thunderbird relies on it).

use chrono::{Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, Offset, TimeZone, Timelike, Weekday};

use crate::lines::{Component, Document, Item, Line};
use crate::vtimezone::VZone;

/// A start or end the way the object states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Moment {
    /// An all-day date (`VALUE=DATE`).
    Date(NaiveDate),
    /// A wall time with no zone: the same clock time wherever the viewer is.
    Floating(NaiveDateTime),
    /// A UTC time (`...Z`).
    Utc(NaiveDateTime),
    /// A wall time in a named zone (`TZID=`).
    Zoned { local: NaiveDateTime, tzid: String },
}

/// Which instance of a recurring event: its original start, in the master
/// `DTSTART`'s own terms (the local time for zoned and floating, the UTC
/// time for UTC, the date for all-day).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Key {
    Date(NaiveDate),
    Time(NaiveDateTime),
}

impl std::fmt::Display for Key {
    /// `20260105` or `20260105T090000`: also the occurrence id the web
    /// app uses.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Date(d) => write!(f, "{}", d.format("%Y%m%d")),
            Key::Time(t) => write!(f, "{}", t.format("%Y%m%dT%H%M%S")),
        }
    }
}

impl std::str::FromStr for Key {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s.len() {
            8 => parse_date(s).map(Key::Date).ok_or(()),
            15 => parse_naive(s).map(Key::Time).ok_or(()),
            _ => Err(()),
        }
    }
}

impl Moment {
    pub fn is_date(&self) -> bool {
        matches!(self, Moment::Date(_))
    }

    /// The instance key of a start in these terms.
    pub fn key(&self) -> Key {
        match self {
            Moment::Date(d) => Key::Date(*d),
            Moment::Floating(t) | Moment::Utc(t) | Moment::Zoned { local: t, .. } => Key::Time(*t),
        }
    }

    /// The same kind of moment at another key (same zone, same form).
    pub fn with_key(&self, key: Key) -> Moment {
        match (self, key) {
            (Moment::Date(_), Key::Date(d)) => Moment::Date(d),
            (Moment::Date(_), Key::Time(t)) => Moment::Date(t.date()),
            (_, Key::Date(d)) => self.with_key(Key::Time(d.and_hms_opt(0, 0, 0).expect("midnight"))),
            (Moment::Floating(_), Key::Time(t)) => Moment::Floating(t),
            (Moment::Utc(_), Key::Time(t)) => Moment::Utc(t),
            (Moment::Zoned { tzid, .. }, Key::Time(t)) => Moment::Zoned { local: t, tzid: tzid.clone() },
        }
    }

    /// Shifted by a duration (dates by whole days).
    pub fn shifted(&self, by: Duration) -> Moment {
        match self.key() {
            Key::Date(d) => Moment::Date(d + Duration::days(by.num_days())),
            Key::Time(t) => self.with_key(Key::Time(t + by)),
        }
    }

    /// Read a `DTSTART` / `DTEND` / `RECURRENCE-ID` style line.
    pub fn from_line(line: &Line) -> Option<Moment> {
        let v = line.value().trim();
        let is_date = line.param("VALUE").is_some_and(|p| p.eq_ignore_ascii_case("DATE")) || v.len() == 8;
        if is_date {
            return parse_date(v).map(Moment::Date);
        }
        if let Some(u) = v.strip_suffix('Z').or_else(|| v.strip_suffix('z')) {
            return parse_naive(u).map(Moment::Utc);
        }
        let local = parse_naive(v)?;
        Some(match line.param("TZID") {
            Some(tzid) => Moment::Zoned { local, tzid },
            None => Moment::Floating(local),
        })
    }

    /// A content line `NAME...:value` for this moment.
    pub fn to_line(&self, name: &str) -> Line {
        Line::new(match self {
            Moment::Date(d) => format!("{name};VALUE=DATE:{}", d.format("%Y%m%d")),
            Moment::Floating(t) => format!("{name}:{}", t.format("%Y%m%dT%H%M%S")),
            Moment::Utc(t) => format!("{name}:{}Z", t.format("%Y%m%dT%H%M%S")),
            Moment::Zoned { local, tzid } => {
                let tzid = if tzid.contains([';', ':', ',']) { format!("\"{tzid}\"") } else { tzid.clone() };
                format!("{name};TZID={tzid}:{}", local.format("%Y%m%dT%H%M%S"))
            }
        })
    }

    /// The value as an `RRULE` `UNTIL` wants it for a series starting with
    /// this kind of moment: a date, a floating time, or UTC for zoned and
    /// UTC starts (RFC 5545 3.3.10). `doc` supplies VTIMEZONEs for TZIDs
    /// the IANA database does not know.
    pub fn until_value(&self, doc: &Document) -> String {
        match self {
            Moment::Date(d) => d.format("%Y%m%d").to_string(),
            Moment::Floating(t) => t.format("%Y%m%dT%H%M%S").to_string(),
            Moment::Utc(t) => format!("{}Z", t.format("%Y%m%dT%H%M%S")),
            Moment::Zoned { local, tzid } => format!("{}Z", zoned_to_utc(*local, tzid, doc).format("%Y%m%dT%H%M%S")),
        }
    }
}

/// A local time in a zone to UTC: the IANA database first, then the
/// object's own VTIMEZONE, else the time as it is.
pub fn zoned_to_utc(local: NaiveDateTime, tzid: &str, doc: &Document) -> NaiveDateTime {
    if let Ok(tz) = tzid.parse::<chrono_tz::Tz>() {
        return match tz.from_local_datetime(&local) {
            LocalResult::Single(t) | LocalResult::Ambiguous(t, _) => t.naive_utc(),
            // In a spring-forward gap: the moment the clock jumps to.
            LocalResult::None => {
                tz.from_local_datetime(&(local + Duration::hours(1))).earliest().map_or(local, |t| t.naive_utc())
            }
        };
    }
    for c in doc.components.iter().flat_map(|cal| cal.components()) {
        if c.name == "VTIMEZONE"
            && c.property("TZID").is_some_and(|l| l.value() == tzid)
            && let Ok(z) = VZone::from_component(c)
        {
            return z.local_to_utc(local);
        }
    }
    local
}

pub fn parse_date(s: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(s, "%Y%m%d").ok()
}

pub fn parse_naive(s: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(s, "%Y%m%dT%H%M%S").ok()
}

/// Whether the calendar already has a VTIMEZONE for `tzid`.
pub fn has_zone(doc: &Document, tzid: &str) -> bool {
    doc.components
        .iter()
        .flat_map(|c| c.components())
        .any(|c| c.name == "VTIMEZONE" && c.property("TZID").is_some_and(|l| l.value() == tzid))
}

/// Add a VTIMEZONE for an IANA zone unless the calendar has one; returns
/// whether one was added. Unknown names are left alone.
pub fn ensure_zone(doc: &mut Document, tzid: &str, year: i32) -> bool {
    if has_zone(doc, tzid) {
        return false;
    }
    let Ok(tz) = tzid.parse::<chrono_tz::Tz>() else { return false };
    let Some(cal) = doc.components.first_mut() else { return false };
    let zone = vtimezone(tz, year);
    // Before the first event component, as writers usually place it.
    let at = cal.items.iter().position(|i| matches!(i, Item::Component(_))).unwrap_or(cal.items.len());
    cal.items.insert(at, Item::Component(zone));
    true
}

/// A VTIMEZONE for an IANA zone as its rules stand in `year`: yearly
/// STANDARD and DAYLIGHT rules by "nth (or last) weekday of the month" when
/// it observes DST, else one fixed STANDARD offset. Exact for the zones
/// with fixed rules (all of Europe and North America); our own expansion
/// reads the IANA database by name anyway.
pub fn vtimezone(tz: chrono_tz::Tz, year: i32) -> Component {
    let mut z = Component::new("VTIMEZONE");
    z.items.push(Item::Property(Line::new(format!("TZID:{}", tz.name()))));
    let offset_at = |t: NaiveDateTime| tz.offset_from_utc_datetime(&t).fix().local_minus_utc();
    let start = NaiveDate::from_ymd_opt(year, 1, 1).expect("jan 1").and_hms_opt(0, 0, 0).expect("midnight");
    // Hourly scan, then minute precision around each change.
    let mut transitions = Vec::new();
    let mut prev = offset_at(start);
    for h in 1..=(366 * 24) {
        let t = start + Duration::hours(h);
        if t.year() != year {
            break;
        }
        let o = offset_at(t);
        if o != prev {
            let mut lo = t - Duration::hours(1);
            let mut hi = t;
            while hi - lo > Duration::minutes(1) {
                let mid = lo + (hi - lo) / 2;
                if offset_at(mid) == prev { lo = mid } else { hi = mid }
            }
            transitions.push((hi, prev, o));
            prev = o;
        }
    }
    let fmt_off = |s: i32| {
        let sign = if s < 0 { '-' } else { '+' };
        let a = s.abs();
        format!("{sign}{:02}{:02}", a / 3600, (a % 3600) / 60)
    };
    if transitions.is_empty() {
        let o = fmt_off(prev);
        let mut s = Component::new("STANDARD");
        for l in ["DTSTART:19700101T000000".to_string(), format!("TZOFFSETFROM:{o}"), format!("TZOFFSETTO:{o}")] {
            s.items.push(Item::Property(Line::new(l)));
        }
        z.items.push(Item::Component(s));
        return z;
    }
    let max_off = transitions.iter().map(|t| t.2.max(t.1)).max().unwrap_or(prev);
    for (utc, from, to) in transitions {
        let local = utc + Duration::seconds(i64::from(from)); // wall clock before the change
        let (month, wd, day) = (local.month(), local.weekday(), local.day());
        let last = NaiveDate::from_ymd_opt(year, month, day).expect("date") + Duration::days(7);
        let nth = if last.month() != month { -1 } else { ((day - 1) / 7 + 1) as i32 };
        let first_1970 = nth_weekday(1970, month, wd, nth);
        let name = if to == max_off && to != from && to > from { "DAYLIGHT" } else { "STANDARD" };
        let mut c = Component::new(name);
        for l in [
            format!("DTSTART:{}T{:02}{:02}00", first_1970.format("%Y%m%d"), local.hour(), local.minute()),
            format!("RRULE:FREQ=YEARLY;BYMONTH={month};BYDAY={nth}{}", weekday_code(wd)),
            format!("TZOFFSETFROM:{}", fmt_off(from)),
            format!("TZOFFSETTO:{}", fmt_off(to)),
        ] {
            c.items.push(Item::Property(Line::new(l)));
        }
        z.items.push(Item::Component(c));
    }
    z
}

fn weekday_code(w: Weekday) -> &'static str {
    match w {
        Weekday::Mon => "MO",
        Weekday::Tue => "TU",
        Weekday::Wed => "WE",
        Weekday::Thu => "TH",
        Weekday::Fri => "FR",
        Weekday::Sat => "SA",
        Weekday::Sun => "SU",
    }
}

/// The nth (1-based, or -1 for last) weekday of a month.
fn nth_weekday(year: i32, month: u32, wd: Weekday, nth: i32) -> NaiveDate {
    if nth > 0 {
        NaiveDate::from_weekday_of_month_opt(year, month, wd, nth as u8).expect("exists")
    } else {
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)
        };
        let mut d = next.expect("date") - Duration::days(1);
        while d.weekday() != wd {
            d -= Duration::days(1);
        }
        d
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> NaiveDateTime {
        parse_naive(s).unwrap()
    }

    #[test]
    fn lines_round_trip() {
        for src in [
            "DTSTART;VALUE=DATE:20260105",
            "DTSTART:20260105T090000",
            "DTSTART:20260105T090000Z",
            "DTSTART;TZID=Europe/Amsterdam:20260105T090000",
        ] {
            let m = Moment::from_line(&Line::new(src)).unwrap();
            assert_eq!(m.to_line("DTSTART").text(), src);
        }
        let quoted = Moment::Zoned { local: dt("20260105T090000"), tzid: "W. Europe; Standard".into() };
        assert_eq!(Moment::from_line(&quoted.to_line("DTSTART")).unwrap(), quoted);
        assert_eq!(
            Moment::from_line(&Line::new("DTSTART:20260105")),
            Some(Moment::Date(parse_date("20260105").unwrap()))
        );
        assert_eq!(Moment::from_line(&Line::new("DTSTART:garbage")), None);
    }

    #[test]
    fn keys_and_shifts() {
        let m = Moment::Zoned { local: dt("20260105T090000"), tzid: "Europe/Amsterdam".into() };
        assert_eq!(m.key().to_string(), "20260105T090000");
        assert_eq!("20260105T090000".parse::<Key>(), Ok(m.key()));
        assert_eq!("20260105".parse::<Key>(), Ok(Key::Date(parse_date("20260105").unwrap())));
        assert!("2026".parse::<Key>().is_err());
        assert_eq!(m.shifted(Duration::hours(2)).key(), Key::Time(dt("20260105T110000")));
        let d = Moment::Date(parse_date("20260105").unwrap());
        assert_eq!(d.shifted(Duration::days(3)), Moment::Date(parse_date("20260108").unwrap()));
    }

    #[test]
    fn until_values() {
        let doc = Document::parse("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n").unwrap();
        let summer = Moment::Zoned { local: dt("20260701T090000"), tzid: "Europe/Amsterdam".into() };
        assert_eq!(summer.until_value(&doc), "20260701T070000Z");
        let winter = Moment::Zoned { local: dt("20260105T090000"), tzid: "Europe/Amsterdam".into() };
        assert_eq!(winter.until_value(&doc), "20260105T080000Z");
        assert_eq!(Moment::Floating(dt("20260105T090000")).until_value(&doc), "20260105T090000");
        assert_eq!(Moment::Date(parse_date("20260105").unwrap()).until_value(&doc), "20260105");
    }

    #[test]
    fn a_generated_zone_evaluates_like_the_iana_database() {
        for name in ["Europe/Amsterdam", "America/New_York", "Australia/Sydney", "Asia/Tokyo"] {
            let tz: chrono_tz::Tz = name.parse().unwrap();
            let z = vtimezone(tz, 2026);
            let v = VZone::from_component(&z).unwrap_or_else(|e| panic!("{name}: {e:?}"));
            // Every 6 hours through 2026 to 2028: the same UTC offset.
            let mut t = dt("20260101T000000");
            while t < dt("20290101T000000") {
                let want = tz.offset_from_utc_datetime(&t).fix().local_minus_utc();
                assert_eq!(v.offset_at_utc(t), want, "{name} at {t}");
                t += Duration::hours(6);
            }
        }
    }

    #[test]
    fn zones_are_added_once() {
        let mut doc =
            Document::parse("BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:a\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n").unwrap();
        assert!(ensure_zone(&mut doc, "Europe/Amsterdam", 2026));
        assert!(!ensure_zone(&mut doc, "Europe/Amsterdam", 2026));
        assert!(!ensure_zone(&mut doc, "Not/AZone", 2026));
        let text = doc.to_string();
        assert!(text.find("BEGIN:VTIMEZONE").unwrap() < text.find("BEGIN:VEVENT").unwrap());
        assert!(text.contains("BYDAY=-1SU") && text.contains("TZOFFSETTO:+0200"));
    }
}
