//! Offsets from a VTIMEZONE's own rules (RFC 5545 section 3.6.5), for a TZID
//! that no name lookup resolves (decision 25: calcard resolves zones only by
//! name).
//!
//! Supported observance shapes cover what calendar clients write: a single
//! onset, RDATE lists, and yearly rules with one BYMONTH plus an nth weekday
//! (`BYDAY=-1SU`) or a weekday inside a BYMONTHDAY window (the old US style
//! `BYDAY=SU;BYMONTHDAY=8,9,10,11,12,13,14`), with optional UNTIL or COUNT.
//! Anything else is `Unsupported`, and the caller falls back.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, Weekday};

use crate::lines::Component;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VZone {
    observances: Vec<Observance>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Observance {
    /// Local onset (in `offset_from`), as DTSTART says.
    start: NaiveDateTime,
    offset_from: i32,
    offset_to: i32,
    rule: Option<Yearly>,
    rdates: Vec<NaiveDateTime>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Yearly {
    month: u32,
    day: DaySpec,
    /// UTC.
    until: Option<NaiveDateTime>,
    count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DaySpec {
    /// The nth (negative: from the end) weekday of the month.
    Nth(i32, Weekday),
    /// The weekday that falls on one of these days of the month.
    InWindow(Weekday, Vec<u32>),
    /// A fixed day of the month.
    MonthDay(u32),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VZoneError {
    #[error("VTIMEZONE has no STANDARD or DAYLIGHT observance")]
    NoObservances,
    #[error("observance {0} is missing or has an invalid DTSTART or TZOFFSET")]
    Invalid(String),
    #[error("observance rule not supported: {0}")]
    Unsupported(String),
}

impl VZone {
    pub fn from_component(vtz: &Component) -> Result<Self, VZoneError> {
        let mut observances = Vec::new();
        for obs in vtz.components().filter(|c| c.name == "STANDARD" || c.name == "DAYLIGHT") {
            let bad = || VZoneError::Invalid(obs.name.clone());
            let start = obs.property("DTSTART").and_then(|l| local_date_time(l.value())).ok_or_else(bad)?;
            let offset_from = obs.property("TZOFFSETFROM").and_then(|l| offset(l.value())).ok_or_else(bad)?;
            let offset_to = obs.property("TZOFFSETTO").and_then(|l| offset(l.value())).ok_or_else(bad)?;
            let rule = match obs.property("RRULE") {
                Some(l) => Some(yearly(l.value())?),
                None => None,
            };
            let mut rdates = Vec::new();
            for l in obs.properties().filter(|l| l.name() == "RDATE") {
                for v in l.value().split(',') {
                    rdates.push(local_date_time(v).ok_or_else(bad)?);
                }
            }
            observances.push(Observance { start, offset_from, offset_to, rule, rdates });
        }
        if observances.is_empty() {
            return Err(VZoneError::NoObservances);
        }
        Ok(Self { observances })
    }

    /// The UTC offset in seconds in effect at a UTC instant.
    pub fn offset_at_utc(&self, utc: NaiveDateTime) -> i32 {
        let latest = self
            .observances
            .iter()
            .filter_map(|o| o.last_onset_utc(utc).map(|t| (t, o.offset_to)))
            .max_by_key(|(t, _)| *t);
        match latest {
            Some((_, off)) => off,
            // Before every onset: the offset the earliest one changes from.
            None => self.observances.iter().min_by_key(|o| o.start).map(|o| o.offset_from).unwrap_or(0),
        }
    }

    /// The UTC instant of a local wall time. In a gap (a time that does not
    /// exist) the offset before the gap is used, so the time moves forward;
    /// in an overlap the first of the two instants wins (RFC 5545 3.3.5).
    pub fn local_to_utc(&self, local: NaiveDateTime) -> NaiveDateTime {
        let mut offsets: Vec<i32> = self.observances.iter().flat_map(|o| [o.offset_from, o.offset_to]).collect();
        offsets.sort_unstable();
        offsets.dedup();
        let mut fits: Vec<NaiveDateTime> = offsets
            .iter()
            .map(|&o| local - Duration::seconds(o as i64))
            .filter(|utc| {
                let back = *utc + Duration::seconds(self.offset_at_utc(*utc) as i64);
                back == local
            })
            .collect();
        fits.sort_unstable();
        match fits.first() {
            Some(utc) => *utc,
            None => {
                let before = self.offset_at_utc(local - Duration::days(1));
                local - Duration::seconds(before as i64)
            }
        }
    }
}

impl Observance {
    /// The latest onset of this observance at or before `utc`, in UTC.
    fn last_onset_utc(&self, utc: NaiveDateTime) -> Option<NaiveDateTime> {
        let to_utc = |local: NaiveDateTime| local - Duration::seconds(self.offset_from as i64);
        let mut best = None;
        let mut consider = |t: NaiveDateTime| {
            if t <= utc && best.is_none_or(|b| t > b) {
                best = Some(t);
            }
        };
        let first = to_utc(self.start);
        consider(first);
        for r in &self.rdates {
            consider(to_utc(*r));
        }
        if let Some(rule) = &self.rule {
            // The answer lies in utc's year or the one before; scanning down
            // from the year after keeps a DST onset near New Year correct.
            let mut year = utc.year() + 1;
            while year >= self.start.year() {
                if let Some(onset) = rule.onset(year, self.start.time()) {
                    let index = (year - self.start.year()) as u32; // COUNT counts years from DTSTART
                    let t = to_utc(onset);
                    let allowed = onset >= self.start
                        && rule.until.is_none_or(|u| t <= u)
                        && rule.count.is_none_or(|c| index < c);
                    if allowed && t <= utc {
                        consider(t);
                        break;
                    }
                }
                year -= 1;
            }
        }
        best
    }
}

impl Yearly {
    fn onset(&self, year: i32, time: chrono::NaiveTime) -> Option<NaiveDateTime> {
        let date = match &self.day {
            DaySpec::Nth(n, wd) => nth_weekday(year, self.month, *n, *wd)?,
            DaySpec::InWindow(wd, days) => days
                .iter()
                .filter_map(|d| NaiveDate::from_ymd_opt(year, self.month, *d))
                .find(|d| d.weekday() == *wd)?,
            DaySpec::MonthDay(d) => NaiveDate::from_ymd_opt(year, self.month, *d)?,
        };
        Some(date.and_time(time))
    }
}

fn nth_weekday(year: i32, month: u32, n: i32, wd: Weekday) -> Option<NaiveDate> {
    if n > 0 {
        let first = NaiveDate::from_ymd_opt(year, month, 1)?;
        let shift = (7 + wd.num_days_from_monday() as i64 - first.weekday().num_days_from_monday() as i64) % 7;
        let d = first + Duration::days(shift + 7 * (n as i64 - 1));
        (d.month() == month).then_some(d)
    } else {
        let next = if month == 12 {
            NaiveDate::from_ymd_opt(year + 1, 1, 1)?
        } else {
            NaiveDate::from_ymd_opt(year, month + 1, 1)?
        };
        let last = next - Duration::days(1);
        let shift = (7 + last.weekday().num_days_from_monday() as i64 - wd.num_days_from_monday() as i64) % 7;
        let d = last - Duration::days(shift + 7 * ((-n) as i64 - 1));
        (d.month() == month).then_some(d)
    }
}

fn yearly(rule: &str) -> Result<Yearly, VZoneError> {
    let unsupported = || VZoneError::Unsupported(rule.to_string());
    let mut freq = None;
    let (mut month, mut byday, mut bymonthday, mut until, mut count) = (None, None, None, None, None);
    for part in rule.split(';') {
        let (k, v) = part.split_once('=').ok_or_else(unsupported)?;
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => freq = Some(v.to_ascii_uppercase()),
            "BYMONTH" => month = Some(v.parse::<u32>().map_err(|_| unsupported())?),
            "BYDAY" => byday = Some(v.to_ascii_uppercase()),
            "BYMONTHDAY" => {
                bymonthday = Some(
                    v.split(',').map(|d| d.parse::<u32>()).collect::<Result<Vec<_>, _>>().map_err(|_| unsupported())?,
                )
            }
            "UNTIL" => until = Some(utc_date_time(v).ok_or_else(unsupported)?),
            "COUNT" => count = Some(v.parse::<u32>().map_err(|_| unsupported())?),
            "INTERVAL" if v == "1" => {}
            "WKST" => {}
            _ => return Err(unsupported()),
        }
    }
    if freq.as_deref() != Some("YEARLY") {
        return Err(unsupported());
    }
    let month = month.ok_or_else(unsupported)?;
    let day = match (byday, bymonthday) {
        (Some(bd), None) => {
            let (n, wd) = bd.split_at(bd.len().saturating_sub(2));
            let wd = weekday(wd).ok_or_else(unsupported)?;
            let n: i32 = n.parse().map_err(|_| unsupported())?;
            if n == 0 || n.abs() > 5 {
                return Err(unsupported());
            }
            DaySpec::Nth(n, wd)
        }
        (Some(bd), Some(days)) => DaySpec::InWindow(weekday(&bd).ok_or_else(unsupported)?, days),
        (None, Some(days)) if days.len() == 1 => DaySpec::MonthDay(days[0]),
        _ => return Err(unsupported()),
    };
    Ok(Yearly { month, day, until, count })
}

fn weekday(s: &str) -> Option<Weekday> {
    Some(match s {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

/// `19961027T030000` (a trailing `Z` is not local, so it is refused).
fn local_date_time(v: &str) -> Option<NaiveDateTime> {
    NaiveDateTime::parse_from_str(v.trim(), "%Y%m%dT%H%M%S").ok()
}

/// `20071104T060000Z`; a date-only UNTIL means the end of that day.
fn utc_date_time(v: &str) -> Option<NaiveDateTime> {
    let v = v.trim();
    if let Some(t) = v.strip_suffix('Z') {
        return NaiveDateTime::parse_from_str(t, "%Y%m%dT%H%M%S").ok();
    }
    if v.len() == 8 {
        return NaiveDate::parse_from_str(v, "%Y%m%d").ok().and_then(|d| d.and_hms_opt(23, 59, 59));
    }
    NaiveDateTime::parse_from_str(v, "%Y%m%dT%H%M%S").ok()
}

/// `+0200`, `-0500`, `+053000` in seconds.
fn offset(v: &str) -> Option<i32> {
    let v = v.trim();
    let (sign, digits) = match v.as_bytes().first()? {
        b'+' => (1, &v[1..]),
        b'-' => (-1, &v[1..]),
        _ => return None,
    };
    if !(digits.len() == 4 || digits.len() == 6) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let h: i32 = digits[0..2].parse().ok()?;
    let m: i32 = digits[2..4].parse().ok()?;
    let s: i32 = if digits.len() == 6 { digits[4..6].parse().ok()? } else { 0 };
    Some(sign * (h * 3600 + m * 60 + s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lines::Document;

    const AMSTERDAM_RULES: &str = "BEGIN:VTIMEZONE\r\nTZID:My Zone\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nDTSTART:19810329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nEND:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nDTSTART:19961027T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";

    fn zone(src: &str) -> VZone {
        VZone::from_component(&Document::parse(src).unwrap().components[0]).unwrap()
    }

    fn dt(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    #[test]
    fn rules_match_the_iana_zone_for_decades() {
        use chrono::{Offset, TimeZone};
        let z = zone(AMSTERDAM_RULES);
        let ams = chrono_tz::Europe::Amsterdam;
        let mut t = dt("1997-01-01 00:00");
        while t < dt("2040-01-01 00:00") {
            let want = ams.offset_from_utc_datetime(&t).fix().local_minus_utc();
            assert_eq!(z.offset_at_utc(t), want, "at {t}");
            t += Duration::hours(7);
        }
    }

    #[test]
    fn gaps_move_forward_and_overlaps_take_the_first() {
        let z = zone(AMSTERDAM_RULES);
        // 2026-03-29 02:30 does not exist: read with +01:00, it is 01:30Z (03:30 CEST).
        assert_eq!(z.local_to_utc(dt("2026-03-29 02:30")), dt("2026-03-29 01:30"));
        // 2026-10-25 02:30 happens twice: first at +02:00.
        assert_eq!(z.local_to_utc(dt("2026-10-25 02:30")), dt("2026-10-25 00:30"));
        assert_eq!(z.local_to_utc(dt("2026-07-01 12:00")), dt("2026-07-01 10:00"));
    }

    #[test]
    fn old_us_window_rules_and_until() {
        let src = "BEGIN:VTIMEZONE\r\nTZID:Old US\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nDTSTART:19870405T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=4;BYDAY=1SU;UNTIL=20060402T070000Z\r\nEND:DAYLIGHT\r\nBEGIN:DAYLIGHT\r\nTZOFFSETFROM:-0500\r\nTZOFFSETTO:-0400\r\nDTSTART:20070311T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=SU;BYMONTHDAY=8,9,10,11,12,13,14\r\nEND:DAYLIGHT\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nDTSTART:20071104T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=11;BYDAY=1SU\r\nEND:STANDARD\r\nBEGIN:STANDARD\r\nTZOFFSETFROM:-0400\r\nTZOFFSETTO:-0500\r\nDTSTART:19671029T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU;UNTIL=20061029T060000Z\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";
        use chrono::{Offset, TimeZone};
        let z = zone(src);
        let ny = chrono_tz::America::New_York;
        let mut t = dt("1990-01-01 00:00");
        while t < dt("2035-01-01 00:00") {
            let want = ny.offset_from_utc_datetime(&t).fix().local_minus_utc();
            assert_eq!(z.offset_at_utc(t), want, "at {t}");
            t += Duration::hours(5);
        }
    }

    #[test]
    fn unsupported_rules_are_refused() {
        let src = AMSTERDAM_RULES.replace("FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU", "FREQ=MONTHLY;BYDAY=-1SU");
        let c = Document::parse(&src).unwrap().components.remove(0);
        assert!(matches!(VZone::from_component(&c), Err(VZoneError::Unsupported(_))));
    }

    #[test]
    fn offsets_parse() {
        assert_eq!(offset("+0200"), Some(7200));
        assert_eq!(offset("-0430"), Some(-16200));
        assert_eq!(offset("+053000"), Some(19800));
        assert_eq!(offset("0200"), None);
    }
}
