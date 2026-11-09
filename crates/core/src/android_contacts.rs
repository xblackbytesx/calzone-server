//! Android's contacts (decision 50): what the system bridge writes into
//! `ContactsContract` for a stored vCard, and how an edit another app made
//! there becomes an edit of the stored text.
//!
//! - [`row`]: the projection, one raw contact in Android's own terms, after
//!   DAVx5's vcard4android (GPL-3.0): the structured name with the shown
//!   name, the organization with the job title, phones, emails and postal
//!   addresses with Android's type numbers (a custom label, the
//!   `item1.X-ABLabel` that DAVx5 and Apple write, is `TYPE_CUSTOM` with that
//!   label), the birthday as Android spells it (`yyyy-MM-dd`, or `--MM-dd`
//!   without a year), the note, the groups (`CATEGORIES`) and an inline
//!   photo. A photo given as a link stays in the stored text only.
//! - [`apply`]: an edited raw contact back to the stored text. It is
//!   compared with what the projection wrote, field by field and by meaning,
//!   and only what changed goes through [`crate::card::apply`], so every
//!   other line (unknown properties, parameters, the label of an untouched
//!   phone) stays byte for byte (decision 5). An entry Android still shows
//!   as it was keeps its own line.
//! - [`new_contact`]: a contact made in another app, as a new vCard.
//!
//! Group cards (`KIND:group`) are not projected. Pure.

use chrono::{NaiveDate, NaiveDateTime};
use serde::{Deserialize, Serialize};

use crate::card::{self, Address, Birthday, CardChange, CardError, CardView, Kind, Photo, Typed};
use crate::lines::{Component, Document, Line};

/// `BaseTypes.TYPE_CUSTOM`: the entry has its own label.
pub const TYPE_CUSTOM: i32 = 0;

/// `CommonDataKinds.Phone.TYPE_*`.
pub mod phone {
    pub const HOME: i32 = 1;
    pub const MOBILE: i32 = 2;
    pub const WORK: i32 = 3;
    pub const FAX_WORK: i32 = 4;
    pub const FAX_HOME: i32 = 5;
    pub const PAGER: i32 = 6;
    pub const OTHER: i32 = 7;
    pub const COMPANY_MAIN: i32 = 10;
    pub const MAIN: i32 = 12;
    pub const OTHER_FAX: i32 = 13;
    pub const WORK_MOBILE: i32 = 17;
    pub const WORK_PAGER: i32 = 18;
}

/// `CommonDataKinds.Email.TYPE_*`.
pub mod email {
    pub const HOME: i32 = 1;
    pub const WORK: i32 = 2;
    pub const OTHER: i32 = 3;
    pub const MOBILE: i32 = 4;
}

/// `CommonDataKinds.StructuredPostal.TYPE_*`.
pub mod postal {
    pub const HOME: i32 = 1;
    pub const WORK: i32 = 2;
    pub const OTHER: i32 = 3;
}

/// A phone or email data row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Entry {
    pub value: String,
    /// Android's type number.
    pub kind: i32,
    /// With [`TYPE_CUSTOM`].
    #[serde(default)]
    pub label: Option<String>,
}

/// A `StructuredPostal` data row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Postal {
    pub kind: i32,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub street: String,
    #[serde(default)]
    pub city: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub postcode: String,
    #[serde(default)]
    pub country: String,
}

/// One raw contact with its data rows, as the bridge writes it and as it
/// reads an edited one back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ContactRow {
    #[serde(default)]
    pub uid: Option<String>,
    /// `StructuredName.DISPLAY_NAME`: FN.
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub given: String,
    #[serde(default)]
    pub middle: String,
    #[serde(default)]
    pub family: String,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub company: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub phones: Vec<Entry>,
    #[serde(default)]
    pub emails: Vec<Entry>,
    #[serde(default)]
    pub addresses: Vec<Postal>,
    /// `Event.START_DATE` of the birthday: `yyyy-MM-dd` or `--MM-dd`.
    #[serde(default)]
    pub birthday: Option<String>,
    #[serde(default)]
    pub note: String,
    /// Group titles.
    #[serde(default)]
    pub groups: Vec<String>,
    /// The picture, base64.
    #[serde(default)]
    pub photo: Option<String>,
}

// ---------------------------------------------------------------------------
// Types.

fn types(line: &Line) -> Vec<String> {
    line.params()
        .into_iter()
        .filter(|p| p.name == "TYPE")
        .flat_map(|p| p.values)
        .flat_map(|v| v.split(',').map(|s| s.trim().to_ascii_lowercase()).collect::<Vec<_>>())
        .collect()
}

/// A label as people read it: Apple's own (`_$!<Mobile>!$_`) without its marks.
fn shown_label(label: &Option<String>) -> Option<String> {
    let l = label.as_deref()?.trim();
    let l = l.strip_prefix("_$!<").and_then(|x| x.strip_suffix(">!$_")).unwrap_or(l);
    (!l.is_empty()).then(|| l.to_string())
}

fn phone_kind(line: &Line, label: &Option<String>) -> i32 {
    if label.is_some() {
        return TYPE_CUSTOM;
    }
    let t = types(line);
    let has = |x: &str| t.iter().any(|y| y == x);
    let (work, home) = (has("work"), has("home"));
    if has("fax") {
        if work {
            phone::FAX_WORK
        } else if home {
            phone::FAX_HOME
        } else {
            phone::OTHER_FAX
        }
    } else if has("pager") {
        if work { phone::WORK_PAGER } else { phone::PAGER }
    } else if has("cell") || has("mobile") || has("iphone") {
        if work { phone::WORK_MOBILE } else { phone::MOBILE }
    } else if work {
        phone::WORK
    } else if home {
        phone::HOME
    } else if has("main") {
        phone::MAIN
    } else {
        phone::OTHER
    }
}

fn email_kind(line: &Line, label: &Option<String>) -> i32 {
    if label.is_some() {
        return TYPE_CUSTOM;
    }
    let t = types(line);
    let has = |x: &str| t.iter().any(|y| y == x);
    if has("work") {
        email::WORK
    } else if has("home") {
        email::HOME
    } else if has("cell") || has("mobile") {
        email::MOBILE
    } else {
        email::OTHER
    }
}

fn postal_kind(line: &Line, label: &Option<String>) -> i32 {
    if label.is_some() {
        return TYPE_CUSTOM;
    }
    let t = types(line);
    if t.iter().any(|y| y == "work") {
        postal::WORK
    } else if t.iter().any(|y| y == "home") {
        postal::HOME
    } else {
        postal::OTHER
    }
}

/// A label for a new line: Android's custom label, or explicitly none.
fn new_label(kind: i32, label: &Option<String>) -> Option<String> {
    let l = if kind == TYPE_CUSTOM { label.as_deref().unwrap_or("").trim() } else { "" };
    Some(l.to_string())
}

fn phone_back(e: &Entry) -> Typed {
    let kind = match e.kind {
        phone::MOBILE | phone::WORK_MOBILE => Kind::Mobile,
        phone::HOME | phone::FAX_HOME => Kind::Home,
        phone::WORK | phone::FAX_WORK | phone::WORK_PAGER | phone::COMPANY_MAIN => Kind::Work,
        _ => Kind::Other,
    };
    Typed { kind, value: e.value.trim().to_string(), label: new_label(e.kind, &e.label) }
}

fn email_back(e: &Entry) -> Typed {
    let kind = match e.kind {
        email::HOME => Kind::Home,
        email::WORK => Kind::Work,
        _ => Kind::Other,
    };
    Typed { kind, value: e.value.trim().to_string(), label: new_label(e.kind, &e.label) }
}

fn postal_back(p: &Postal) -> Address {
    let kind = match p.kind {
        postal::HOME => Some(Kind::Home),
        postal::WORK => Some(Kind::Work),
        _ => None,
    };
    Address {
        kind,
        street: p.street.trim().to_string(),
        city: p.city.trim().to_string(),
        region: p.region.trim().to_string(),
        postcode: p.postcode.trim().to_string(),
        country: p.country.trim().to_string(),
        label: new_label(p.kind, &p.label),
    }
}

/// What two entries mean to Android: the label counts only for a custom type.
fn entry_key(e: &Entry) -> (i32, String, String) {
    let label = if e.kind == TYPE_CUSTOM { e.label.as_deref().unwrap_or("").trim().to_string() } else { String::new() };
    (e.kind, label, e.value.trim().to_string())
}

fn postal_key(p: &Postal) -> (i32, String, [String; 5]) {
    let label = if p.kind == TYPE_CUSTOM { p.label.as_deref().unwrap_or("").trim().to_string() } else { String::new() };
    let f = |s: &str| s.trim().to_string();
    (p.kind, label, [f(&p.street), f(&p.city), f(&p.region), f(&p.postcode), f(&p.country)])
}

fn same_set<T, K: Ord>(a: &[T], b: &[T], key: impl Fn(&T) -> K) -> bool {
    let mut x: Vec<K> = a.iter().map(&key).collect();
    let mut y: Vec<K> = b.iter().map(&key).collect();
    x.sort();
    y.sort();
    x == y
}

/// The edited list as the card's own entries: one Android still shows as
/// projected keeps the card's entry (and so its line); the rest are new.
fn back<A, B: Clone, K: PartialEq>(
    projected: &[A],
    originals: &[B],
    edited: &[A],
    key: impl Fn(&A) -> K,
    fresh: impl Fn(&A) -> B,
) -> Vec<B> {
    let mut used = vec![false; projected.len()];
    edited
        .iter()
        .map(|e| {
            let k = key(e);
            match (0..projected.len()).find(|&i| !used[i] && key(&projected[i]) == k) {
                Some(i) => {
                    used[i] = true;
                    originals[i].clone()
                }
                None => fresh(e),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Birthdays and photos.

fn birthday_text(b: Birthday) -> String {
    match b {
        Birthday::Full(d) => d.format("%Y-%m-%d").to_string(),
        Birthday::NoYear { month, day } => format!("--{month:02}-{day:02}"),
    }
}

/// Android's spellings: `yyyy-MM-dd`, `--MM-dd`, `yyyyMMdd`, with or without a time.
fn parse_birthday(s: &str) -> Option<Birthday> {
    let s = s.trim();
    let s = s.split(['T', ' ']).next().unwrap_or(s);
    if let Some(rest) = s.strip_prefix("--") {
        let d: String = rest.chars().filter(char::is_ascii_digit).collect();
        if d.len() != 4 {
            return None;
        }
        let (month, day) = (d[..2].parse().ok()?, d[2..].parse().ok()?);
        NaiveDate::from_ymd_opt(2000, month, day)?;
        return Some(Birthday::NoYear { month, day });
    }
    let d: String = s.chars().filter(char::is_ascii_digit).collect();
    (d.len() == 8).then(|| NaiveDate::parse_from_str(&d, "%Y%m%d").ok().map(Birthday::Full)).flatten()
}

fn squash(s: &str) -> String {
    s.split_whitespace().collect()
}

fn photo_mime(base64: &str) -> &'static str {
    if base64.starts_with("iVBOR") {
        "image/png"
    } else if base64.starts_with("R0lGOD") {
        "image/gif"
    } else {
        "image/jpeg"
    }
}

// ---------------------------------------------------------------------------
// The projection.

struct Projected {
    row: ContactRow,
    /// The card's own entries, one per projected entry.
    phones: Vec<Typed>,
    emails: Vec<Typed>,
    addresses: Vec<Address>,
}

fn is_group(c: &Component) -> bool {
    ["KIND", "X-ADDRESSBOOKSERVER-KIND"]
        .iter()
        .any(|n| c.property(n).is_some_and(|l| l.value().trim().eq_ignore_ascii_case("group")))
}

fn project(text: &str) -> Option<Projected> {
    let doc = Document::parse(text).ok()?;
    let c = match doc.components.as_slice() {
        [c] if c.name == "VCARD" => c,
        _ => return None,
    };
    if is_group(c) {
        return None;
    }
    let v: CardView = card::read(text).ok()?;
    let lines = |name: &str| c.properties().filter(|l| l.name() == name).collect::<Vec<_>>();
    let (mut phones, mut phone_rows) = (Vec::new(), Vec::new());
    for (line, t) in lines("TEL").into_iter().zip(&v.phones) {
        if t.value.trim().is_empty() {
            continue;
        }
        let label = shown_label(&t.label);
        phone_rows.push(Entry { value: t.value.trim().to_string(), kind: phone_kind(line, &label), label });
        phones.push(t.clone());
    }
    let (mut emails, mut email_rows) = (Vec::new(), Vec::new());
    for (line, t) in lines("EMAIL").into_iter().zip(&v.emails) {
        if t.value.trim().is_empty() {
            continue;
        }
        let label = shown_label(&t.label);
        email_rows.push(Entry { value: t.value.trim().to_string(), kind: email_kind(line, &label), label });
        emails.push(t.clone());
    }
    let (mut addresses, mut postal_rows) = (Vec::new(), Vec::new());
    for (line, a) in lines("ADR").into_iter().zip(&v.addresses) {
        if a.is_empty() {
            continue;
        }
        let label = shown_label(&a.label);
        postal_rows.push(Postal {
            kind: postal_kind(line, &label),
            label,
            street: a.street.trim().to_string(),
            city: a.city.trim().to_string(),
            region: a.region.trim().to_string(),
            postcode: a.postcode.trim().to_string(),
            country: a.country.trim().to_string(),
        });
        addresses.push(a.clone());
    }
    let row = ContactRow {
        uid: v.uid.clone(),
        display_name: v.name.trim().to_string(),
        prefix: v.prefix.trim().to_string(),
        given: v.given.trim().to_string(),
        middle: v.middle.trim().to_string(),
        family: v.family.trim().to_string(),
        suffix: v.suffix.trim().to_string(),
        company: v.org.trim().to_string(),
        title: v.title.trim().to_string(),
        phones: phone_rows,
        emails: email_rows,
        addresses: postal_rows,
        birthday: v.birthday.map(birthday_text),
        note: v.note.trim().to_string(),
        groups: v.categories.clone(),
        photo: match &v.photo {
            Some(Photo::Inline { base64, .. }) => Some(squash(base64)),
            _ => None,
        },
    };
    Some(Projected { row, phones, emails, addresses })
}

/// The raw contact for a stored vCard, or `None` when it is not a contact
/// Android holds (it does not parse, or it is a group card).
pub fn row(text: &str) -> Option<ContactRow> {
    project(text).map(|p| p.row)
}

fn name_parts(r: &ContactRow) -> [&str; 5] {
    [r.prefix.trim(), r.given.trim(), r.middle.trim(), r.family.trim(), r.suffix.trim()]
}

/// An edited raw contact (`edited`) back to the stored text. Only what
/// changed by meaning becomes a change; nothing changed returns the text
/// as it was.
pub fn apply(text: &str, edited: &ContactRow, now: NaiveDateTime) -> Result<String, CardError> {
    let p = project(text).ok_or(CardError::NotACard)?;
    let b = &p.row;
    let mut ch = CardChange::default();
    let differs = |x: &str, y: &str| x.trim() != y.trim();
    // The names. A card without N parts gets them from Android, which takes
    // them apart from the shown name: only a new shown name is a change then.
    let (pb, pe) = (name_parts(b), name_parts(edited));
    let no_parts = pb.iter().all(|s| s.is_empty()) && !differs(&edited.display_name, &b.display_name);
    if pb != pe && !no_parts {
        ch.prefix = Some(edited.prefix.trim().to_string());
        ch.given = Some(edited.given.trim().to_string());
        ch.middle = Some(edited.middle.trim().to_string());
        ch.family = Some(edited.family.trim().to_string());
        ch.suffix = Some(edited.suffix.trim().to_string());
    } else if !edited.display_name.trim().is_empty() && differs(&edited.display_name, &b.display_name) {
        ch.name = Some(edited.display_name.trim().to_string());
    }
    if differs(&edited.company, &b.company) {
        ch.org = Some(edited.company.trim().to_string());
    }
    if differs(&edited.title, &b.title) {
        ch.title = Some(edited.title.trim().to_string());
    }
    if !same_set(&b.phones, &edited.phones, entry_key) {
        ch.phones = Some(back(&b.phones, &p.phones, &edited.phones, entry_key, phone_back));
    }
    if !same_set(&b.emails, &edited.emails, entry_key) {
        ch.emails = Some(back(&b.emails, &p.emails, &edited.emails, entry_key, email_back));
    }
    if !same_set(&b.addresses, &edited.addresses, postal_key) {
        ch.addresses = Some(back(&b.addresses, &p.addresses, &edited.addresses, postal_key, postal_back));
    }
    let was = b.birthday.as_deref().and_then(parse_birthday);
    match edited.birthday.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
        None if was.is_some() => ch.birthday = Some(None),
        // A date Android wrote in a form we do not read is left alone.
        Some(s) => {
            if let Some(d) = parse_birthday(s)
                && Some(d) != was
            {
                ch.birthday = Some(Some(d));
            }
        }
        None => {}
    }
    if differs(&edited.note, &b.note) {
        ch.note = Some(edited.note.trim().to_string());
    }
    let set = |v: &[String]| {
        let mut s: Vec<String> = v.iter().map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
        s.sort();
        s.dedup();
        s
    };
    if set(&b.groups) != set(&edited.groups) {
        ch.categories = Some(edited.groups.clone());
    }
    let photo = edited.photo.as_deref().map(squash).filter(|s| !s.is_empty());
    if photo != b.photo {
        ch.photo = Some(photo.map(|base64| Photo::Inline { mime: photo_mime(&base64).into(), base64 }));
    }
    if ch == CardChange::default() {
        return Ok(text.to_string());
    }
    card::apply(text, &ch, now)
}

/// A raw contact another app made, as a new vCard with `uid`. A contact
/// with no name takes its organization, a phone number or an email address
/// as the name shown, else `fallback_name`.
pub fn new_contact(row: &ContactRow, uid: &str, now: NaiveDateTime, fallback_name: &str) -> Result<String, CardError> {
    let named = name_parts(row).iter().any(|s| !s.is_empty()) || !row.company.trim().is_empty();
    let shown = [row.display_name.as_str()]
        .into_iter()
        .chain((!named).then_some(row.phones.first().map_or("", |e| e.value.as_str())))
        .chain((!named).then_some(row.emails.first().map_or("", |e| e.value.as_str())))
        .map(str::trim)
        .find(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| (!named).then(|| fallback_name.trim().to_string()));
    let some = |s: &str| Some(s.trim().to_string()).filter(|s| !s.is_empty());
    let change = CardChange {
        name: shown,
        given: some(&row.given),
        family: some(&row.family),
        middle: some(&row.middle),
        prefix: some(&row.prefix),
        suffix: some(&row.suffix),
        org: some(&row.company),
        title: some(&row.title),
        phones: Some(row.phones.iter().map(phone_back).collect()),
        emails: Some(row.emails.iter().map(email_back).collect()),
        addresses: Some(row.addresses.iter().map(postal_back).collect()),
        birthday: Some(row.birthday.as_deref().and_then(parse_birthday)),
        note: some(&row.note),
        categories: Some(row.groups.clone()),
        photo: Some(
            row.photo
                .as_deref()
                .map(squash)
                .filter(|s| !s.is_empty())
                .map(|base64| Photo::Inline { mime: photo_mime(&base64).into(), base64 }),
        ),
    };
    card::new_card(&change, uid, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 9).unwrap().and_hms_opt(12, 0, 0).unwrap()
    }

    /// The shape of the household's export (vCard 4, DAVx5's labels).
    const V4: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nPRODID:-//x//y//EN\r\nUID:ada\r\nFN:Ada Lovelace\r\n\
N:Lovelace;Ada;Augusta;;\r\nORG:Analytical;Engines\r\nTITLE:Countess\r\n\
TEL;TYPE=cell:+44 7700 900001\r\nTEL;TYPE=work,fax:+44 20 7946 0002\r\n\
item1.TEL:+44 20 7946 0003\r\nitem1.X-ABLabel:Lab\r\n\
item2.EMAIL;TYPE=home:ada@example.org\r\nitem2.X-ABLabel:_$!<Other>!$_\r\nEMAIL;TYPE=work:ada@work.example\r\n\
ADR;TYPE=home:;;12 St James Sq;London;;SW1Y 4JH;UK\r\nBDAY:18151210\r\nNOTE:Poet\\nof science\r\n\
CATEGORIES:Family,Science\r\nX-CUSTOM;FOO=bar:keep\r\nPHOTO:data:image/jpeg;base64,/9j/4AAQ\r\n SkZJRg==\r\n\
REV:20260101T000000Z\r\nEND:VCARD\r\n";

    #[test]
    fn projection() {
        let r = row(V4).unwrap();
        assert_eq!(r.uid.as_deref(), Some("ada"));
        assert_eq!(
            (r.display_name.as_str(), r.given.as_str(), r.middle.as_str(), r.family.as_str()),
            ("Ada Lovelace", "Ada", "Augusta", "Lovelace")
        );
        assert_eq!((r.company.as_str(), r.title.as_str()), ("Analytical, Engines", "Countess"));
        let phones: Vec<_> = r.phones.iter().map(|e| (e.kind, e.label.as_deref())).collect();
        assert_eq!(phones, vec![(phone::MOBILE, None), (phone::FAX_WORK, None), (TYPE_CUSTOM, Some("Lab"))]);
        let emails: Vec<_> = r.emails.iter().map(|e| (e.kind, e.label.as_deref())).collect();
        assert_eq!(emails, vec![(TYPE_CUSTOM, Some("Other")), (email::WORK, None)], "Apple's marks are dropped");
        assert_eq!((r.addresses[0].kind, r.addresses[0].city.as_str()), (postal::HOME, "London"));
        assert_eq!(r.birthday.as_deref(), Some("1815-12-10"));
        assert_eq!(r.note, "Poet\nof science");
        assert_eq!(r.groups, vec!["Family".to_string(), "Science".to_string()]);
        assert_eq!(r.photo.as_deref(), Some("/9j/4AAQSkZJRg=="));
        let group = "BEGIN:VCARD\r\nVERSION:4.0\r\nKIND:group\r\nFN:Team\r\nEND:VCARD\r\n";
        assert_eq!(row(group), None);
    }

    #[test]
    fn unchanged_is_no_edit() {
        let r = row(V4).unwrap();
        assert_eq!(apply(V4, &r, now()).unwrap(), V4);
        // Android's own order and spacing are no change either.
        let mut shuffled = r.clone();
        shuffled.phones.reverse();
        shuffled.groups.reverse();
        shuffled.note = " Poet\nof science ".into();
        shuffled.photo = Some("/9j/4AAQ\nSkZJRg==".into());
        assert_eq!(apply(V4, &shuffled, now()).unwrap(), V4);
    }

    #[test]
    fn one_change_touches_its_lines_only() {
        let mut r = row(V4).unwrap();
        r.phones.push(Entry { value: "+31 6 1234".into(), kind: phone::HOME, label: None });
        let out = apply(V4, &r, now()).unwrap();
        for keep in [
            "TEL;TYPE=work,fax:+44 20 7946 0002",
            "item1.TEL:+44 20 7946 0003",
            "item1.X-ABLabel:Lab",
            "item2.X-ABLabel:_$!<Other>!$_",
            "X-CUSTOM;FOO=bar:keep",
            "ORG:Analytical;Engines",
        ] {
            assert!(out.contains(&format!("{keep}\r\n")), "{keep}");
        }
        assert!(out.contains("TEL;TYPE=home:+31 6 1234\r\n") && out.contains("REV:20261109T120000Z"));
        assert_eq!(apply(&out, &row(&out).unwrap(), now()).unwrap(), out, "and reads back unchanged");

        // A custom label from Android.
        let mut r = row(V4).unwrap();
        r.emails[1] = Entry { value: "ada@work.example".into(), kind: TYPE_CUSTOM, label: Some("Old job".into()) };
        let out = apply(V4, &r, now()).unwrap();
        assert!(out.contains("item3.EMAIL:ada@work.example\r\nitem3.X-ABLabel:Old job\r\n"), "{out}");
        assert!(!out.contains("EMAIL;TYPE=work") && out.contains("item2.EMAIL;TYPE=home:ada@example.org"));

        // A labeled phone turned into a plain mobile loses its label.
        let mut r = row(V4).unwrap();
        r.phones[2] = Entry { value: "+44 20 7946 0003".into(), kind: phone::MOBILE, label: Some("Lab".into()) };
        let out = apply(V4, &r, now()).unwrap();
        assert!(out.contains("TEL;TYPE=cell:+44 20 7946 0003\r\n") && !out.contains("item1."), "{out}");
    }

    #[test]
    fn names_titles_dates_groups_and_photos() {
        let mut r = row(V4).unwrap();
        r.family = "King".into();
        r.display_name = "Ada Augusta King".into(); // Android recomputes it
        let out = apply(V4, &r, now()).unwrap();
        assert!(out.contains("N:King;Ada;Augusta;;\r\n") && out.contains("FN:Ada King\r\n"), "{out}");

        let mut r = row(V4).unwrap();
        r.display_name = "Countess Ada".into();
        let out = apply(V4, &r, now()).unwrap();
        assert!(out.contains("FN:Countess Ada\r\n") && out.contains("N:Lovelace;Ada;Augusta;;\r\n"));

        // No N in the card: Android's parts taken from the shown name are no edit.
        let bare = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:b\r\nFN:Bea Smith\r\nEND:VCARD\r\n";
        let mut r = row(bare).unwrap();
        (r.given, r.family) = ("Bea".into(), "Smith".into());
        assert_eq!(apply(bare, &r, now()).unwrap(), bare);

        let mut r = row(V4).unwrap();
        r.title = "".into();
        r.company = "Engines Ltd".into();
        r.birthday = Some("--12-10".into());
        r.groups = vec!["Family".into(), "Friends".into()];
        let out = apply(V4, &r, now()).unwrap();
        assert!(!out.contains("TITLE") && out.contains("ORG:Engines Ltd\r\n"));
        assert!(out.contains("BDAY:--1210\r\n") && out.contains("CATEGORIES:Family,Friends\r\n"));

        let mut r = row(V4).unwrap();
        r.birthday = None;
        r.photo = Some("iVBORw0K".into());
        let out = apply(V4, &r, now()).unwrap();
        assert!(!out.contains("BDAY") && out.contains("PHOTO:data:image/png;base64,iVBORw0K\r\n"));
        r.photo = None;
        assert!(!apply(V4, &r, now()).unwrap().contains("PHOTO"));
    }

    #[test]
    fn new_contacts() {
        let r = ContactRow {
            display_name: "Bea Smith".into(),
            given: "Bea".into(),
            family: "Smith".into(),
            phones: vec![Entry { value: "+31 6 1".into(), kind: phone::MOBILE, label: None }],
            emails: vec![Entry { value: "bea@example.org".into(), kind: TYPE_CUSTOM, label: Some("Club".into()) }],
            birthday: Some("1990-03-15".into()),
            groups: vec!["Friends".into()],
            ..Default::default()
        };
        let text = new_contact(&r, "u1", now(), "(no name)").unwrap();
        assert!(text.contains("UID:u1") && text.contains("FN:Bea Smith") && text.contains("N:Smith;Bea;;;"));
        assert!(text.contains("TEL;TYPE=CELL:+31 6 1") && text.contains("item1.EMAIL:bea@example.org"));
        assert!(text.contains("item1.X-ABLabel:Club") && text.contains("BDAY:1990-03-15"));
        assert!(text.contains("CATEGORIES:Friends"));
        assert_eq!(apply(&text, &row(&text).unwrap(), now()).unwrap(), text, "reads back unchanged");

        let only_phone = ContactRow { phones: r.phones.clone(), ..Default::default() };
        assert!(new_contact(&only_phone, "u2", now(), "(no name)").unwrap().contains("FN:+31 6 1\r\n"));
        assert!(new_contact(&ContactRow::default(), "u3", now(), "(no name)").unwrap().contains("FN:(no name)\r\n"));
    }
}
