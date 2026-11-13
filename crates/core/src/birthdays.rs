//! Birthdays from contacts (decision 57): a yearly all-day event for every
//! birthday (and, when asked, wedding anniversary) on the cards a person
//! reads, the same person on several cards shown once, and the manual
//! birthday events people made before, found so they can go.
//!
//! The same person: the same occasion on the same day of the year, years
//! that do not disagree, and the same name (folded as in [`dedupe`]), phone
//! number or email address. Two cards for one person with different
//! birthdays stay two events, which shows that one of them is wrong.
//!
//! The age is not in the title (it changes every year, and a stored title
//! would need a new copy each year): the event carries the year it counts
//! from in `X-CALZONE-SINCE`, and [`crate::view::occurrences`] adds the age
//! for each year it shows. Other apps see the plain name.

use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};

use crate::card::{self, Birthday, CardView};
use crate::dedupe::{email_key, fold, name_key, phone_key};
use crate::lines::Document;

/// The property holding the year an event's age counts from.
pub const SINCE: &str = "X-CALZONE-SINCE";
/// The property naming the card an event came from (`<book>/<name>`), only
/// when its owner may read that card.
pub const CONTACT: &str = "X-CALZONE-CONTACT";

/// Years before this are placeholders for "year unknown" (Apple writes 1604).
const FIRST_REAL_YEAR: i32 = 1800;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Occasion {
    Birthday,
    Anniversary,
}

impl Occasion {
    pub fn as_str(self) -> &'static str {
        match self {
            Occasion::Birthday => "birthday",
            Occasion::Anniversary => "anniversary",
        }
    }
}

/// A day of the year, with the year when it is known.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Day {
    pub month: u32,
    pub day: u32,
    pub year: Option<i32>,
}

impl Day {
    pub fn of(b: Birthday) -> Day {
        match b {
            Birthday::Full(d) if d.year() >= FIRST_REAL_YEAR => {
                Day { month: d.month(), day: d.day(), year: Some(d.year()) }
            }
            Birthday::Full(d) => Day { month: d.month(), day: d.day(), year: None },
            Birthday::NoYear { month, day } => Day { month, day, year: None },
        }
    }

    pub fn birthday(self) -> Birthday {
        match self.year.and_then(|y| NaiveDate::from_ymd_opt(y, self.month, self.day)) {
            Some(d) => Birthday::Full(d),
            None => Birthday::NoYear { month: self.month, day: self.day },
        }
    }
}

/// One card's dates and what identifies its person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
    pub view: CardView,
    pub birthday: Option<Day>,
    pub anniversary: Option<Day>,
    /// Lower is preferred when one person is on several cards (the
    /// person's own address books before others').
    pub rank: u32,
    /// Where the card is, opaque here: the event's UID is made from it.
    pub source: String,
}

/// The wedding anniversary on a card: `ANNIVERSARY` (vCard 4),
/// `X-ANNIVERSARY` (vCard 3 writers), or Apple's labeled `X-ABDATE`.
fn anniversary(text: &str) -> Option<Birthday> {
    let doc = Document::parse(text).ok()?;
    let c = doc.components.first()?;
    if let Some(b) =
        ["ANNIVERSARY", "X-ANNIVERSARY"].iter().find_map(|n| c.property(n).and_then(|l| card::birthday_of(l.value())))
    {
        return Some(b);
    }
    c.properties().filter(|l| l.name() == "X-ABDATE").find_map(|l| {
        let g = l.group()?;
        let labeled = c.properties().any(|x| {
            x.name() == "X-ABLABEL"
                && x.group().is_some_and(|xg| xg.eq_ignore_ascii_case(g))
                && x.value().to_ascii_lowercase().contains("anniversary")
        });
        labeled.then(|| card::birthday_of(l.value())).flatten()
    })
}

/// A card read for birthdays; `None` when it is not a card or has neither
/// date.
pub fn person(text: &str, rank: u32, source: String) -> Option<Person> {
    let view = card::read(text).ok()?;
    let birthday = view.birthday.map(Day::of);
    let anniversary = anniversary(text).map(Day::of);
    if birthday.is_none() && anniversary.is_none() {
        return None;
    }
    Some(Person { view, birthday, anniversary, rank, source })
}

/// The name a card goes by: its shown name, else given and family name,
/// else its organization.
pub fn shown_name(v: &CardView) -> String {
    let n = v.name.trim();
    if !n.is_empty() {
        return n.to_string();
    }
    let gf = format!("{} {}", v.given.trim(), v.family.trim()).trim().to_string();
    if !gf.is_empty() {
        return gf;
    }
    v.org.trim().to_string()
}

/// One event of the Birthdays calendar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub occasion: Occasion,
    pub day: Day,
    /// The card it is shown from, an index into the people given.
    pub person: usize,
    /// Every card that is this person on this day.
    pub cards: Vec<usize>,
    pub name: String,
    pub uid: String,
}

fn keys(v: &CardView) -> Vec<String> {
    let mut k: Vec<String> = Vec::new();
    k.extend(name_key(v).map(|n| format!("n:{n}")));
    k.extend(v.phones.iter().filter_map(|p| phone_key(&p.value)).map(|p| format!("p:{p}")));
    k.extend(v.emails.iter().filter_map(|e| email_key(&e.value)).map(|e| format!("e:{e}")));
    k
}

/// A stable 64-bit FNV-1a hash, for UIDs that stay the same between
/// rebuilds (no randomness in the core).
fn fnv(s: &str) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// The events for these cards: one per person and occasion, in date order.
/// Anniversaries only when asked. Cards without a usable name are left out.
pub fn entries(people: &[Person], anniversaries: bool) -> Vec<Entry> {
    // (occasion, month, day) -> cards on it.
    let mut by_day: BTreeMap<(Occasion, u32, u32), Vec<(usize, Day)>> = BTreeMap::new();
    for (i, p) in people.iter().enumerate() {
        if shown_name(&p.view).is_empty() {
            continue;
        }
        if let Some(d) = p.birthday {
            by_day.entry((Occasion::Birthday, d.month, d.day)).or_default().push((i, d));
        }
        if anniversaries && let Some(d) = p.anniversary {
            by_day.entry((Occasion::Anniversary, d.month, d.day)).or_default().push((i, d));
        }
    }
    let mut out = Vec::new();
    for ((occasion, _, _), cards) in by_day {
        // Union by shared keys, never joining two different known years.
        let n = cards.len();
        let mut root: Vec<usize> = (0..n).collect();
        let mut year: Vec<Option<i32>> = cards.iter().map(|(_, d)| d.year).collect();
        fn find(root: &mut [usize], i: usize) -> usize {
            let mut r = i;
            while root[r] != r {
                r = root[r];
            }
            root[i] = r;
            r
        }
        let ks: Vec<Vec<String>> = cards.iter().map(|(i, _)| keys(&people[*i].view)).collect();
        for a in 0..n {
            for b in a + 1..n {
                if !ks[a].iter().any(|k| ks[b].contains(k)) {
                    continue;
                }
                let (ra, rb) = (find(&mut root, a), find(&mut root, b));
                if ra == rb {
                    continue;
                }
                match (year[ra], year[rb]) {
                    (Some(x), Some(y)) if x != y => continue,
                    (x, y) => {
                        root[rb] = ra;
                        year[ra] = x.or(y);
                    }
                }
            }
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for a in 0..n {
            let r = find(&mut root, a);
            groups.entry(r).or_default().push(a);
        }
        for (r, members) in groups {
            let shown = *members
                .iter()
                .min_by_key(|&&m| {
                    let (i, d) = cards[m];
                    (people[i].rank, d.year.is_none(), i)
                })
                .expect("a group has members");
            let (pi, d) = cards[shown];
            let day = Day { year: year[r], ..d };
            let uid = format!("{}-{:016x}@calzone", occasion.as_str(), fnv(&people[pi].source));
            out.push(Entry {
                occasion,
                day,
                person: pi,
                cards: members.iter().map(|&m| cards[m].0).collect(),
                name: shown_name(&people[pi].view),
                uid,
            });
        }
    }
    out
}

/// The event for an entry, titled `title`, linking `contact` when given.
/// Nothing in it changes unless the card does, so rebuilds write nothing.
pub fn event(e: &Entry, title: &str, contact: Option<&str>) -> String {
    let feb29 = e.day.month == 2 && e.day.day == 29;
    // A leap year for the 29th; an old one otherwise, so every year shows.
    let year = e.day.year.unwrap_or(if feb29 { 1972 } else { 1970 });
    let start = NaiveDate::from_ymd_opt(year, e.day.month, e.day.day)
        .or_else(|| NaiveDate::from_ymd_opt(1972, e.day.month, e.day.day))
        .unwrap_or_default();
    let end = start.succ_opt().unwrap_or(start);
    // The 29th of February falls on the 28th in other years.
    let rule = if feb29 { "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=-1" } else { "FREQ=YEARLY" };
    let mut s = String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//CalZone//Birthdays//EN\r\nBEGIN:VEVENT\r\n");
    s.push_str(&format!("UID:{}\r\n", e.uid));
    s.push_str("DTSTAMP:20000101T000000Z\r\n");
    s.push_str(&format!("DTSTART;VALUE=DATE:{}\r\n", start.format("%Y%m%d")));
    s.push_str(&format!("DTEND;VALUE=DATE:{}\r\n", end.format("%Y%m%d")));
    s.push_str(&format!("RRULE:{rule}\r\n"));
    s.push_str(&format!("SUMMARY:{}\r\n", card::escape(title)));
    s.push_str("TRANSP:TRANSPARENT\r\n");
    if let Some(y) = e.day.year {
        s.push_str(&format!("{SINCE}:{y}\r\n"));
    }
    if let Some(c) = contact {
        s.push_str(&format!("{CONTACT}:{}\r\n", card::escape(c)));
    }
    s.push_str("END:VEVENT\r\nEND:VCALENDAR\r\n");
    s
}

/// The age (years since) for an occurrence in `year`, when the event
/// carries one and it is at least 1.
pub fn age(since: Option<i32>, year: i32) -> Option<i32> {
    since.map(|s| year - s).filter(|a| *a >= 1)
}

/// A manual yearly all-day event, as people made them for birthdays.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manual {
    pub title: String,
    /// Its first day: the day of the year, and the year it starts.
    pub start: NaiveDate,
}

/// The event when it is one yearly all-day event that never ends and has
/// no changed occurrences: the shape of a birthday made by hand. Not one
/// of the Birthdays calendar's own.
pub fn manual(text: &str) -> Option<Manual> {
    let doc = Document::parse(text).ok()?;
    let cal = doc.components.first()?;
    let evs: Vec<_> = cal.components().filter(|c| c.name == "VEVENT").collect();
    let [ev] = evs.as_slice() else { return None };
    if ev.property("RECURRENCE-ID").is_some() || ev.property(SINCE).is_some() {
        return None;
    }
    let rule = ev.property("RRULE")?.value().to_ascii_uppercase();
    let parts: Vec<&str> = rule.split(';').collect();
    if !parts.contains(&"FREQ=YEARLY")
        || parts.iter().any(|p| {
            p.starts_with("COUNT=") || p.starts_with("UNTIL=") || p.starts_with("INTERVAL=") && *p != "INTERVAL=1"
        })
    {
        return None;
    }
    let start = ev.property("DTSTART")?;
    let v = start.value().trim();
    let date_only = start.param("VALUE").is_some_and(|x| x.eq_ignore_ascii_case("DATE")) || v.len() == 8;
    if !date_only {
        return None;
    }
    let start = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
    let title = ev.property("SUMMARY").map(|l| card::unescape(l.value())).unwrap_or_default();
    Some(Manual { title, start })
}

/// Whether an event title names this card's person: every word of the
/// card's name (folded) is a word of the title.
pub fn names(title: &str, v: &CardView) -> bool {
    let Some(key) = name_key(v) else { return false };
    let words: Vec<String> =
        fold(title).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    key.split(' ').all(|k| words.iter().any(|w| w == k))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(fn_: &str, extra: &str) -> String {
        format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{fn_}\r\nFN:{fn_}\r\n{extra}END:VCARD\r\n")
    }

    fn p(text: &str, rank: u32, source: &str) -> Person {
        person(text, rank, source.into()).expect("has a date")
    }

    #[test]
    fn dates_read() {
        let a = p(&card("Ada", "BDAY:19851203\r\nX-ANNIVERSARY:2010-06-05\r\n"), 0, "a");
        assert_eq!(a.birthday, Some(Day { month: 12, day: 3, year: Some(1985) }));
        assert_eq!(a.anniversary, Some(Day { month: 6, day: 5, year: Some(2010) }));
        let b = p(&card("Bo", "BDAY;X-APPLE-OMIT-YEAR=1604:1604-03-04\r\n"), 0, "b");
        assert_eq!(b.birthday, Some(Day { month: 3, day: 4, year: None }));
        let c = p(
            &card("Cy", "BDAY:--0229\r\nitem1.X-ABDATE:2001-05-01\r\nitem1.X-ABLabel:_$!<Anniversary>!$_\r\n"),
            0,
            "c",
        );
        assert_eq!(c.birthday, Some(Day { month: 2, day: 29, year: None }));
        assert_eq!(c.anniversary, Some(Day { month: 5, day: 1, year: Some(2001) }));
        assert!(person(&card("Dee", "TEL:123\r\n"), 0, "d".into()).is_none());
    }

    #[test]
    fn the_same_person_once() {
        let people = vec![
            // Mine and my partner's card for one person: same day, one by
            // phone only (different names), one by name.
            p(&card("Oma", "TEL:+31 6 1111 2222\r\nBDAY:19400101\r\n"), 0, "mine/oma"),
            p(&card("Gerda de Vries", "TEL:06-11112222\r\nBDAY:--0101\r\n"), 1, "hers/gerda"),
            p(&card("Anna Smit", "BDAY:19900505\r\n"), 1, "hers/anna"),
            p(&card("Smit, Anna", "BDAY:19900505\r\n"), 0, "mine/anna"),
            // Same name and day, different years: two people (or a mistake).
            p(&card("Jan Bakker", "BDAY:19700707\r\n"), 0, "mine/jan"),
            p(&card("Jan Bakker", "BDAY:19710707\r\n"), 1, "hers/jan"),
            // Same day, nothing shared: two people.
            p(&card("Piet", "BDAY:19800505\r\n"), 0, "mine/piet"),
        ];
        let e = entries(&people, false);
        let names: Vec<(&str, Option<i32>)> = e.iter().map(|x| (x.name.as_str(), x.day.year)).collect();
        assert_eq!(
            names,
            vec![
                ("Oma", Some(1940)),
                ("Smit, Anna", Some(1990)),
                ("Piet", Some(1980)),
                ("Jan Bakker", Some(1970)),
                ("Jan Bakker", Some(1971))
            ]
        );
        assert_eq!(e[0].cards, vec![0, 1]);
        // The UID follows the shown card, so it is stable between rebuilds.
        assert_eq!(e[1].uid, entries(&people, false)[1].uid);
        assert!(e[1].uid.starts_with("birthday-"));
    }

    #[test]
    fn year_known_only_once_is_kept() {
        let people = vec![
            p(&card("Bo Li", "BDAY:--0304\r\n"), 0, "mine/bo"),
            p(&card("Bo Li", "BDAY:19990304\r\n"), 1, "hers/bo"),
        ];
        let e = entries(&people, false);
        assert_eq!(e.len(), 1);
        assert_eq!((e[0].person, e[0].day.year), (0, Some(1999)));
    }

    #[test]
    fn anniversaries_when_asked() {
        let people = vec![p(&card("Ada", "ANNIVERSARY:20100605\r\n"), 0, "a")];
        assert!(entries(&people, false).is_empty());
        let e = entries(&people, true);
        assert_eq!((e[0].occasion, e[0].uid.starts_with("anniversary-")), (Occasion::Anniversary, true));
    }

    #[test]
    fn events_and_ages() {
        let people = vec![p(&card("Ada", "BDAY:19851203\r\n"), 0, "a"), p(&card("Leap", "BDAY:--0229\r\n"), 0, "b")];
        let e = entries(&people, false);
        let ada = event(&e[1], "Ada, the poet", Some("book/ada.vcf"));
        assert!(ada.contains("DTSTART;VALUE=DATE:19851203\r\nDTEND;VALUE=DATE:19851204\r\nRRULE:FREQ=YEARLY\r\n"));
        assert!(ada.contains("SUMMARY:Ada\\, the poet\r\n"));
        assert!(ada.contains("X-CALZONE-SINCE:1985\r\nX-CALZONE-CONTACT:book/ada.vcf\r\n"));
        let leap = event(&e[0], "Leap", None);
        assert!(leap.contains("DTSTART;VALUE=DATE:19720229\r\n") && leap.contains("BYMONTHDAY=-1"));
        assert!(!leap.contains("X-CALZONE-SINCE") && !leap.contains("X-CALZONE-CONTACT"));
        let from = |y: i32, m: u32, d: u32| NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(0, 0, 0).unwrap();
        let in_2026 = crate::recur::expand(&leap, from(2026, 1, 1), from(2027, 1, 1), chrono_tz::UTC).unwrap();
        assert_eq!(in_2026.occurrences.len(), 1);
        assert_eq!(
            in_2026.occurrences[0].start,
            crate::recur::When::Date(NaiveDate::from_ymd_opt(2026, 2, 28).unwrap())
        );
        let in_2028 = crate::recur::expand(&leap, from(2028, 1, 1), from(2029, 1, 1), chrono_tz::UTC).unwrap();
        assert_eq!(
            in_2028.occurrences[0].start,
            crate::recur::When::Date(NaiveDate::from_ymd_opt(2028, 2, 29).unwrap())
        );
        assert_eq!((age(Some(1985), 2026), age(Some(2026), 2026), age(None, 2026)), (Some(41), None, None));
        // The grid's title carries the age of the year it shows.
        let shown = crate::view::occurrences(&ada, from(2026, 12, 1), from(2027, 12, 31), chrono_tz::UTC);
        let titles: Vec<&str> = shown.iter().map(|o| o.title.as_str()).collect();
        assert_eq!(titles, vec!["Ada, the poet (41)", "Ada, the poet (42)"]);
        let first = crate::view::occurrences(&ada, from(1985, 1, 1), from(1986, 1, 1), chrono_tz::UTC);
        assert_eq!(first[0].title, "Ada, the poet");
    }

    #[test]
    fn manual_birthdays() {
        let ev = |props: &str| {
            format!(
                "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTAMP:20200101T000000Z\r\n{props}END:VEVENT\r\nEND:VCALENDAR\r\n"
            )
        };
        let m = manual(&ev(
            "DTSTART;VALUE=DATE:19851203\r\nRRULE:FREQ=YEARLY;BYMONTH=12\r\nSUMMARY:Verjaardag Ada Lovelace\r\n",
        ))
        .unwrap();
        assert_eq!(
            (m.title.as_str(), m.start),
            ("Verjaardag Ada Lovelace", NaiveDate::from_ymd_opt(1985, 12, 3).unwrap())
        );
        assert!(manual(&ev("DTSTART:19851203T090000Z\r\nRRULE:FREQ=YEARLY\r\n")).is_none(), "timed");
        assert!(manual(&ev("DTSTART;VALUE=DATE:19851203\r\nRRULE:FREQ=YEARLY;COUNT=3\r\n")).is_none(), "ends");
        assert!(manual(&ev("DTSTART;VALUE=DATE:19851203\r\n")).is_none(), "once");
        let ada = card::read(&card("Ada Lovelace", "")).unwrap();
        assert!(names("Verjaardag Ada Lovelace", &ada));
        assert!(names("Lovelace, Ada (40)", &ada));
        assert!(!names("Ada's party", &ada));
    }
}
