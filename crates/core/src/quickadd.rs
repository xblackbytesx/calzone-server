//! Quick add (decision 12): "dentist thu 3pm" or "tandarts do 15u" into a
//! title and a time. Pure: today and the language are passed in, and the
//! result is wall-clock time in the person's own zone (the server makes it
//! zoned). English and Dutch (decision 35); the person's language decides,
//! because words collide ("do" is Thursday in Dutch, "van" is "from").
//!
//! Understood: today/tomorrow (vandaag, morgen, overmorgen), tonight
//! (vanavond, 19:00 unless a time is given), weekdays with or without
//! "next" (volgende), dates ("24 dec", "dec 24", "3 maart", "24/12",
//! Dutch "24-12", "2026-12-24"), day ranges ("24-28 dec"), times ("3pm",
//! "15:30", "15.30", "15u", "15u30", "at 3", "om 3"), time ranges ("3-4pm",
//! "15:00-16:30", "from 8pm to 11pm", "van 14 tot 15u"), and lengths
//! ("1h", "90m", "2 hours", "1,5u", "45 minuten"). What is left is the
//! title.

use chrono::{Datelike, Duration, NaiveDate, NaiveDateTime, NaiveTime, Weekday};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Nl,
}

/// When the event is, in the person's own wall-clock time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuickWhen {
    /// All day; `end` is exclusive (the day after the last day).
    Days {
        start: NaiveDate,
        end: NaiveDate,
    },
    Time {
        start: NaiveDateTime,
        end: NaiveDateTime,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub title: String,
    /// None: no day or time was found; the form opens with the title.
    pub when: Option<QuickWhen>,
}

const DEFAULT_LENGTH_MIN: i64 = 60;
const EVENING: (u32, u32) = (19, 0);

struct Words {
    today: &'static [&'static str],
    tomorrow: &'static [&'static str],
    after_tomorrow: &'static [&'static str],
    tonight: &'static [&'static str],
    next: &'static [&'static str],
    /// Dropped when they stand right before a date or time ("at", "om").
    fillers: &'static [&'static str],
    /// Between two times ("to", "tot").
    until: &'static [&'static str],
    weekdays: [(&'static [&'static str], Weekday); 7],
    months: [&'static [&'static str]; 12],
    hours: &'static [&'static str],
    minutes: &'static [&'static str],
    /// Whether "24-12" is a date (day-month).
    dash_dates: bool,
}

const EN: Words = Words {
    today: &["today"],
    tomorrow: &["tomorrow", "tmrw", "tmr"],
    after_tomorrow: &[],
    tonight: &["tonight"],
    next: &["next"],
    fillers: &["at", "on", "from", "for", "in"],
    until: &["to", "until", "till", "-"],
    weekdays: [
        (&["mon", "monday"], Weekday::Mon),
        (&["tue", "tues", "tuesday"], Weekday::Tue),
        (&["wed", "weds", "wednesday"], Weekday::Wed),
        (&["thu", "thur", "thurs", "thursday"], Weekday::Thu),
        (&["fri", "friday"], Weekday::Fri),
        (&["sat", "saturday"], Weekday::Sat),
        (&["sun", "sunday"], Weekday::Sun),
    ],
    months: [
        &["jan", "january"],
        &["feb", "february"],
        &["mar", "march"],
        &["apr", "april"],
        &["may"],
        &["jun", "june"],
        &["jul", "july"],
        &["aug", "august"],
        &["sep", "sept", "september"],
        &["oct", "october"],
        &["nov", "november"],
        &["dec", "december"],
    ],
    hours: &["h", "hr", "hrs", "hour", "hours"],
    minutes: &["m", "min", "mins", "minute", "minutes"],
    dash_dates: false,
};

const NL: Words = Words {
    today: &["vandaag"],
    tomorrow: &["morgen"],
    after_tomorrow: &["overmorgen"],
    tonight: &["vanavond"],
    next: &["volgende", "aanstaande"],
    fillers: &["om", "op", "van", "voor", "vanaf"],
    until: &["tot", "-"],
    weekdays: [
        (&["ma", "maandag"], Weekday::Mon),
        (&["di", "dinsdag"], Weekday::Tue),
        (&["wo", "woensdag"], Weekday::Wed),
        (&["do", "donderdag"], Weekday::Thu),
        (&["vr", "vrij", "vrijdag"], Weekday::Fri),
        (&["za", "zaterdag"], Weekday::Sat),
        (&["zo", "zondag"], Weekday::Sun),
    ],
    months: [
        &["jan", "januari"],
        &["feb", "februari"],
        &["mrt", "maart"],
        &["apr", "april"],
        &["mei"],
        &["jun", "juni"],
        &["jul", "juli"],
        &["aug", "augustus"],
        &["sep", "sept", "september"],
        &["okt", "oktober"],
        &["nov", "november"],
        &["dec", "december"],
    ],
    hours: &["u", "uur", "h"],
    minutes: &["m", "min", "minuut", "minuten"],
    dash_dates: true,
};

/// A clock time as typed: hour, minute, and am/pm if given.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Clock {
    h: u32,
    m: u32,
    meridiem: Option<bool>, // Some(true) = pm
    /// Typed as a plain number ("3"): taken as a time only after "at"/"om",
    /// before "uur" or in a range, and 1 to 7 then mean the afternoon.
    bare: bool,
}

impl Clock {
    fn resolve(self) -> Option<NaiveTime> {
        let h = match self.meridiem {
            Some(pm) => {
                if !(1..=12).contains(&self.h) {
                    return None;
                }
                (self.h % 12) + if pm { 12 } else { 0 }
            }
            // "at 3": afternoon for 1 to 7, as people say it.
            None if self.bare && (1..=7).contains(&self.h) => self.h + 12,
            None => self.h,
        };
        NaiveTime::from_hms_opt(h, self.m, 0)
    }
}

fn clean(tok: &str) -> String {
    tok.trim_end_matches([',', '!', '?', ';']).trim_end_matches('.').to_lowercase()
}

fn digits(s: &str) -> Option<u32> {
    (!s.is_empty() && s.len() <= 4 && s.bytes().all(|b| b.is_ascii_digit())).then(|| s.parse().ok()).flatten()
}

/// One token as a clock time.
fn clock(tok: &str, w: &Words) -> Option<Clock> {
    let t = tok;
    // am/pm suffix (English only, but harmless in Dutch).
    for (suffix, pm) in [("am", false), ("pm", true), ("a", false), ("p", true)] {
        if let Some(rest) = t.strip_suffix(suffix)
            && let Some(c) = hm(rest)
        {
            return Some(Clock { meridiem: Some(pm), bare: false, ..c });
        }
    }
    // Dutch 15u, 15u30, 15h.
    for marker in ["u", "h"] {
        if let Some((h, m)) = t.split_once(marker)
            && w.hours.contains(&marker)
            && let Some(h) = digits(h).filter(|h| *h < 24)
        {
            let m = if m.is_empty() { Some(0) } else { digits(m).filter(|m| *m < 60) };
            if let Some(m) = m {
                return Some(Clock { h, m, meridiem: None, bare: false });
            }
        }
    }
    hm(t)
}

/// `15:30`, `15.30`, `3` (bare).
fn hm(t: &str) -> Option<Clock> {
    if let Some((h, m)) = t.split_once([':', '.']) {
        let (h, m) = (digits(h)?, digits(m)?);
        return (h < 24 && m < 60 && t.len() <= 5).then_some(Clock { h, m, meridiem: None, bare: false });
    }
    let h = digits(t)?;
    (h < 24 && t.len() <= 2).then_some(Clock { h, m: 0, meridiem: None, bare: true })
}

fn month(tok: &str, w: &Words) -> Option<u32> {
    w.months.iter().position(|names| names.contains(&tok)).map(|i| i as u32 + 1)
}

fn weekday(tok: &str, w: &Words) -> Option<Weekday> {
    w.weekdays.iter().find(|(names, _)| names.contains(&tok)).map(|(_, d)| *d)
}

/// A length: `1h`, `1.5h`, `1,5u`, `90m`, `90min`.
fn length_one(tok: &str, w: &Words) -> Option<Duration> {
    let split = tok.find(|c: char| c.is_ascii_alphabetic())?;
    let (num, unit) = tok.split_at(split);
    let n: f64 = num.replace(',', ".").parse().ok()?;
    if n <= 0.0 || n > 1000.0 {
        return None;
    }
    if w.hours.contains(&unit) {
        Some(Duration::minutes((n * 60.0).round() as i64))
    } else if w.minutes.contains(&unit) {
        Some(Duration::minutes(n.round() as i64))
    } else {
        None
    }
}

fn length_two(num: &str, unit: &str, w: &Words) -> Option<Duration> {
    let n: f64 = num.replace(',', ".").parse().ok()?;
    if n <= 0.0 || n > 1000.0 {
        return None;
    }
    if w.hours.contains(&unit) && unit.len() > 1 {
        Some(Duration::minutes((n * 60.0).round() as i64))
    } else if w.minutes.contains(&unit) && unit.len() > 1 {
        Some(Duration::minutes(n.round() as i64))
    } else {
        None
    }
}

/// The next date with this month and day on or after `today`.
fn upcoming(today: NaiveDate, month: u32, day: u32, year: Option<i32>) -> Option<NaiveDate> {
    if let Some(y) = year {
        return NaiveDate::from_ymd_opt(y, month, day);
    }
    let this = NaiveDate::from_ymd_opt(today.year(), month, day);
    match this {
        Some(d) if d >= today => Some(d),
        _ => NaiveDate::from_ymd_opt(today.year() + 1, month, day),
    }
}

fn next_weekday(today: NaiveDate, wd: Weekday, strictly_after: bool) -> NaiveDate {
    let mut d = if strictly_after { today + Duration::days(1) } else { today };
    while d.weekday() != wd {
        d += Duration::days(1);
    }
    d
}

fn year_token(tok: &str) -> Option<i32> {
    digits(tok).filter(|y| (1900..=2200).contains(y)).map(|y| y as i32)
}

/// `24/12`, `24/12/2026`, Dutch `24-12`, `24-12-2026`, ISO `2026-12-24`.
fn numeric_date(tok: &str, today: NaiveDate, w: &Words) -> Option<NaiveDate> {
    // ISO (2026-12-24) means the same in every language.
    let iso: Vec<&str> = tok.split('-').collect();
    if let [y, m, d] = iso.as_slice()
        && y.len() == 4
    {
        return NaiveDate::from_ymd_opt(digits(y)? as i32, digits(m)?, digits(d)?);
    }
    let seps: &[char] = if w.dash_dates { &['/', '-'] } else { &['/'] };
    let parts: Vec<&str> = tok.split(seps).collect();
    match parts.as_slice() {
        [d, m] => upcoming(today, digits(m)?, digits(d)?, None),
        [d, m, y] => {
            let y = digits(y)?;
            let y = if y < 100 { 2000 + y } else { y };
            NaiveDate::from_ymd_opt(y as i32, digits(m)?, digits(d)?)
        }
        _ => None,
    }
}

#[derive(Default)]
struct Found {
    date: Option<NaiveDate>,
    last_date: Option<NaiveDate>,
    start: Option<Clock>,
    end: Option<Clock>,
    length: Option<Duration>,
    evening: bool,
}

/// Read a quick-add line. `now` is the person's local wall-clock time.
pub fn parse(input: &str, lang: Lang, now: NaiveDateTime) -> Option<Draft> {
    let w = match lang {
        Lang::En => &EN,
        Lang::Nl => &NL,
    };
    let today = now.date();
    let raw: Vec<&str> = input.split_whitespace().collect();
    let toks: Vec<String> = raw.iter().map(|t| clean(t)).collect();
    let mut used = vec![false; toks.len()];
    let mut f = Found::default();
    let mut i = 0;
    while i < toks.len() {
        let t = toks[i].as_str();
        let next = toks.get(i + 1).map(String::as_str);
        let after = toks.get(i + 2).map(String::as_str);
        let take = |n: usize, used: &mut Vec<bool>| {
            for u in used.iter_mut().skip(i).take(n) {
                *u = true;
            }
            n
        };
        // Day words.
        if f.date.is_none() {
            if w.today.contains(&t) {
                f.date = Some(today);
                i += take(1, &mut used);
                continue;
            }
            if w.tomorrow.contains(&t) {
                f.date = Some(today + Duration::days(1));
                i += take(1, &mut used);
                continue;
            }
            if w.after_tomorrow.contains(&t) {
                f.date = Some(today + Duration::days(2));
                i += take(1, &mut used);
                continue;
            }
            if w.tonight.contains(&t) {
                f.date = Some(today);
                f.evening = true;
                i += take(1, &mut used);
                continue;
            }
            if lang == Lang::En && t == "day" && next == Some("after") && after == Some("tomorrow") {
                f.date = Some(today + Duration::days(2));
                i += take(3, &mut used);
                continue;
            }
            if w.next.contains(&t)
                && let Some(wd) = next.and_then(|n| weekday(n, w))
            {
                f.date = Some(next_weekday(today, wd, true));
                i += take(2, &mut used);
                continue;
            }
            if let Some(wd) = weekday(t, w) {
                f.date = Some(next_weekday(today, wd, false));
                i += take(1, &mut used);
                continue;
            }
            // "24-28 dec": a day range.
            if let Some((a, b)) = t.split_once('-')
                && let (Some(a), Some(b)) = (digits(a), digits(b))
                && let Some(m) = next.and_then(|n| month(n, w))
            {
                let year = after.and_then(year_token);
                if let (Some(s), Some(e)) = (upcoming(today, m, a, year), upcoming(today, m, b, year)) {
                    let e =
                        if e < s { NaiveDate::from_ymd_opt(e.year() + 1, e.month(), e.day()).unwrap_or(e) } else { e };
                    f.date = Some(s);
                    f.last_date = Some(e);
                    i += take(2 + usize::from(year.is_some()), &mut used);
                    continue;
                }
            }
            // "24 dec [2026]" and "dec 24 [2026]".
            if let (Some(d), Some(m)) = (digits(t), next.and_then(|n| month(n, w)))
                && d <= 31
            {
                let year = after.and_then(year_token);
                if let Some(date) = upcoming(today, m, d, year) {
                    f.date = Some(date);
                    i += take(2 + usize::from(year.is_some()), &mut used);
                    continue;
                }
            }
            if let (Some(m), Some(d)) = (month(t, w), next.and_then(digits))
                && d <= 31
            {
                let year = after.and_then(year_token);
                if let Some(date) = upcoming(today, m, d, year) {
                    f.date = Some(date);
                    i += take(2 + usize::from(year.is_some()), &mut used);
                    continue;
                }
            }
            if (t.contains('/') || w.dash_dates && t.contains('-') || t.len() == 10)
                && let Some(date) = numeric_date(t, today, w)
            {
                f.date = Some(date);
                i += take(1, &mut used);
                continue;
            }
        }
        // Times.
        if f.start.is_none() {
            // "3-4pm", "15:00-16:30", "9u-10u30".
            if let Some((a, b)) = t.split_once('-')
                && let (Some(a), Some(b)) = (clock(a, w), clock(b, w))
                && !(a.bare && b.bare && w.dash_dates)
            {
                let a = if a.meridiem.is_none() && b.meridiem.is_some() { inherit(a, b) } else { a };
                f.start = Some(Clock { bare: false, ..a });
                f.end = Some(Clock { bare: false, ..b });
                i += take(1, &mut used);
                continue;
            }
            let after_filler = i > 0 && w.fillers.contains(&toks[i - 1].as_str());
            let unit_next = next.is_some_and(|n| n == "uur" || n == "o'clock");
            if let Some(c) = clock(t, w)
                && (!c.bare || after_filler || unit_next || next.is_some_and(|n| w.until.contains(&n)))
            {
                // A range written as "3pm to 5pm" / "van 14 tot 15u".
                if let Some(u) = next
                    && w.until.contains(&u)
                    && let Some(e) = after.and_then(|a| clock(a, w))
                {
                    let c = if c.meridiem.is_none() && e.meridiem.is_some() { inherit(c, e) } else { c };
                    f.start = Some(Clock { bare: false, ..c });
                    f.end = Some(Clock { bare: false, ..e });
                    i += take(3, &mut used);
                    continue;
                }
                // "3 uur": the unit word belongs to the time. A bare number
                // keeps `bare`, so "at 3" reads as the afternoon.
                let unit = next.is_some_and(|n| n == "uur" || n == "o'clock");
                f.start = Some(c);
                i += take(1 + usize::from(unit), &mut used);
                continue;
            }
        }
        // Lengths.
        if f.length.is_none() {
            if let Some(d) = length_one(t, w) {
                f.length = Some(d);
                i += take(1, &mut used);
                continue;
            }
            if let Some(d) = next.and_then(|u| length_two(t, u, w)) {
                f.length = Some(d);
                i += take(2, &mut used);
                continue;
            }
        }
        i += 1;
    }
    // Fillers right before something taken go too ("at 3", "om 15u").
    for k in 0..toks.len() {
        if !used[k] && w.fillers.contains(&toks[k].as_str()) && used.get(k + 1).copied().unwrap_or(false) {
            used[k] = true;
        }
    }
    let title: String = raw
        .iter()
        .zip(&used)
        .filter(|(_, u)| !**u)
        .map(|(t, _)| *t)
        .collect::<Vec<_>>()
        .join(" ")
        .trim_matches(|c: char| c == ',' || c == '-' || c.is_whitespace())
        .to_string();
    if title.is_empty() {
        return None;
    }
    let start_time = f
        .start
        .and_then(Clock::resolve)
        .or_else(|| f.evening.then(|| NaiveTime::from_hms_opt(EVENING.0, EVENING.1, 0)).flatten());
    let when = match (f.date, start_time) {
        (None, None) => None,
        (Some(d), None) => {
            let last = f.last_date.unwrap_or(d);
            Some(QuickWhen::Days { start: d, end: last + Duration::days(1) })
        }
        (date, Some(t)) => {
            let day = date.unwrap_or(if t > now.time() { today } else { today + Duration::days(1) });
            let start = day.and_time(t);
            let end = match f.end.and_then(Clock::resolve) {
                Some(e) => {
                    let e = day.and_time(e);
                    if e <= start { e + Duration::days(1) } else { e }
                }
                None => start + f.length.unwrap_or_else(|| Duration::minutes(DEFAULT_LENGTH_MIN)),
            };
            Some(QuickWhen::Time { start, end })
        }
    };
    Some(Draft { title, when })
}

/// "3-4pm": the start takes the end's am/pm, unless that would put it
/// after the end ("11-1pm" is 11am to 1pm).
fn inherit(start: Clock, end: Clock) -> Clock {
    let pm = end.meridiem;
    let with = Clock { meridiem: pm, bare: false, ..start };
    match (with.resolve(), end.resolve()) {
        (Some(s), Some(e)) if s > e => Clock { meridiem: pm.map(|p| !p), bare: false, ..start },
        _ => with,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tuesday 6 January 2026, 10:00.
    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 1, 6).unwrap().and_hms_opt(10, 0, 0).unwrap()
    }

    fn at(m: u32, d: u32, h: u32, min: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, m, d).unwrap().and_hms_opt(h, min, 0).unwrap()
    }

    fn timed(input: &str, lang: Lang) -> (String, NaiveDateTime, NaiveDateTime) {
        let d = parse(input, lang, now()).unwrap_or_else(|| panic!("{input}: nothing"));
        match d.when {
            Some(QuickWhen::Time { start, end }) => (d.title, start, end),
            other => panic!("{input}: {other:?}"),
        }
    }

    fn days(input: &str, lang: Lang) -> (String, NaiveDate, NaiveDate) {
        let d = parse(input, lang, now()).unwrap_or_else(|| panic!("{input}: nothing"));
        match d.when {
            Some(QuickWhen::Days { start, end }) => (d.title, start, end),
            other => panic!("{input}: {other:?}"),
        }
    }

    fn date(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    #[test]
    fn english() {
        assert_eq!(timed("dentist thu 3pm", Lang::En), ("dentist".into(), at(1, 8, 15, 0), at(1, 8, 16, 0)));
        assert_eq!(
            timed("lunch with Sam tomorrow 12:30 1h", Lang::En),
            ("lunch with Sam".into(), at(1, 7, 12, 30), at(1, 7, 13, 30))
        );
        assert_eq!(timed("Call mum at 3", Lang::En), ("Call mum".into(), at(1, 6, 15, 0), at(1, 6, 16, 0)));
        assert_eq!(timed("standup 9:15", Lang::En).1, at(1, 7, 9, 15), "already past today: tomorrow");
        assert_eq!(
            timed("party on fri from 8pm to 11pm", Lang::En),
            ("party".into(), at(1, 9, 20, 0), at(1, 9, 23, 0))
        );
        assert_eq!(timed("meeting 3-4pm next mon", Lang::En), ("meeting".into(), at(1, 12, 15, 0), at(1, 12, 16, 0)));
        assert_eq!(timed("review 11-1pm wed", Lang::En), ("review".into(), at(1, 7, 11, 0), at(1, 7, 13, 0)));
        assert_eq!(timed("flight 22:00-01:00 sat", Lang::En), ("flight".into(), at(1, 10, 22, 0), at(1, 11, 1, 0)));
        assert_eq!(timed("run tue 7am 45 min", Lang::En), ("run".into(), at(1, 6, 7, 0), at(1, 6, 7, 45)));
        assert_eq!(timed("next tue 9:00 call", Lang::En).1, at(1, 13, 9, 0), "next: never today");
        assert_eq!(timed("dinner tonight", Lang::En), ("dinner".into(), at(1, 6, 19, 0), at(1, 6, 20, 0)));
        assert_eq!(days("holiday 24-28 dec", Lang::En), ("holiday".into(), date(2026, 12, 24), date(2026, 12, 29)));
        assert_eq!(
            days("Mia's birthday 3 March", Lang::En),
            ("Mia's birthday".into(), date(2026, 3, 3), date(2026, 3, 4))
        );
        assert_eq!(days("trip jan 2", Lang::En).1, date(2027, 1, 2), "a past date means next year");
        assert_eq!(days("expo 24/12", Lang::En).1, date(2026, 12, 24));
        assert_eq!(days("move 2026-03-15", Lang::En).1, date(2026, 3, 15));
        assert_eq!(days("do taxes tomorrow", Lang::En), ("do taxes".into(), date(2026, 1, 7), date(2026, 1, 8)));
        assert_eq!(
            days("conference 3-5 may 2027", Lang::En),
            ("conference".into(), date(2027, 5, 3), date(2027, 5, 6))
        );
    }

    #[test]
    fn dutch() {
        assert_eq!(timed("tandarts do 15u", Lang::Nl), ("tandarts".into(), at(1, 8, 15, 0), at(1, 8, 16, 0)));
        assert_eq!(
            timed("lunch met Sam morgen 12:30 1u", Lang::Nl),
            ("lunch met Sam".into(), at(1, 7, 12, 30), at(1, 7, 13, 30))
        );
        assert_eq!(timed("bellen om 3", Lang::Nl), ("bellen".into(), at(1, 6, 15, 0), at(1, 6, 16, 0)));
        assert_eq!(timed("etentje vanavond", Lang::Nl), ("etentje".into(), at(1, 6, 19, 0), at(1, 6, 20, 0)));
        assert_eq!(
            timed("zwemmen volgende ma 9u-10u30", Lang::Nl),
            ("zwemmen".into(), at(1, 12, 9, 0), at(1, 12, 10, 30))
        );
        assert_eq!(
            timed("vergadering van 14:00 tot 15:00 vr", Lang::Nl),
            ("vergadering".into(), at(1, 9, 14, 0), at(1, 9, 15, 0))
        );
        assert_eq!(timed("training wo 19.30 1,5u", Lang::Nl), ("training".into(), at(1, 7, 19, 30), at(1, 7, 21, 0)));
        assert_eq!(timed("ouderavond do om 7 uur", Lang::Nl), ("ouderavond".into(), at(1, 8, 19, 0), at(1, 8, 20, 0)));
        assert_eq!(
            timed("kapper overmorgen 11u 45 minuten", Lang::Nl),
            ("kapper".into(), at(1, 8, 11, 0), at(1, 8, 11, 45))
        );
        assert_eq!(days("vakantie 24-28 dec", Lang::Nl), ("vakantie".into(), date(2026, 12, 24), date(2026, 12, 29)));
        assert_eq!(
            days("verjaardag Mia 3 maart", Lang::Nl),
            ("verjaardag Mia".into(), date(2026, 3, 3), date(2026, 3, 4))
        );
        assert_eq!(days("afspraak 12-3", Lang::Nl).1, date(2026, 3, 12), "day-month in Dutch");
        assert_eq!(days("koningsdag 27 april", Lang::Nl).1, date(2026, 4, 27));
    }

    #[test]
    fn words_that_collide_stay_in_the_title() {
        let d = parse("rent a van tomorrow", Lang::En, now()).unwrap();
        assert_eq!(d.title, "rent a van");
        let d = parse("do the dishes", Lang::En, now()).unwrap();
        assert_eq!((d.title.as_str(), d.when), ("do the dishes", None));
        let d = parse("plan 3 trips", Lang::En, now()).unwrap();
        assert_eq!((d.title.as_str(), d.when), ("plan 3 trips", None), "a bare number is not a time");
        let d = parse("Mei en Sam", Lang::En, now()).unwrap();
        assert_eq!(d.when, None, "\"mei\" is May only in Dutch");
        assert_eq!(parse("tomorrow 3pm", Lang::En, now()), None, "no title");
        assert_eq!(parse("", Lang::Nl, now()), None);
    }
}
