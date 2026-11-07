//! Contacts (phase 2 step 4): a vCard (3.0 or 4.0) read into the fields
//! people edit, and a change set applied losslessly like events (decision
//! 25): only the lines a change touches are rewritten; unknown properties,
//! parameters, groups (`item1.TEL`) and photos stay as they were. For the
//! repeating fields (phones, emails, addresses) a line whose type and value
//! are still wanted is kept byte for byte; the rest go, and new ones are
//! added after the last line of their kind.

use chrono::{Datelike, NaiveDate, NaiveDateTime};

use crate::lines::{Component, Document, Item, Line};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Mobile,
    Home,
    Work,
    Other,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Mobile => "mobile",
            Kind::Home => "home",
            Kind::Work => "work",
            Kind::Other => "other",
        }
    }

    pub fn parse(s: &str) -> Kind {
        match s {
            "mobile" => Kind::Mobile,
            "home" => Kind::Home,
            "work" => Kind::Work,
            _ => Kind::Other,
        }
    }

    /// From a line's TYPE parameters (any case, comma lists or repeats).
    fn of(line: &Line) -> Kind {
        let types: Vec<String> = line
            .params()
            .into_iter()
            .filter(|p| p.name == "TYPE")
            .flat_map(|p| p.values)
            .flat_map(|v| v.split(',').map(|s| s.trim().to_ascii_lowercase()).collect::<Vec<_>>())
            .collect();
        let has = |t: &str| types.iter().any(|x| x == t);
        if has("cell") || has("mobile") {
            Kind::Mobile
        } else if has("work") {
            Kind::Work
        } else if has("home") {
            Kind::Home
        } else {
            Kind::Other
        }
    }

    /// The TYPE to write for a new line, in the card's version's spelling.
    fn param(self, v4: bool, phone: bool) -> Option<&'static str> {
        match (self, v4) {
            (Kind::Mobile, false) if phone => Some("CELL"),
            (Kind::Mobile, true) if phone => Some("cell"),
            (Kind::Mobile, _) => None,
            (Kind::Home, false) => Some("HOME"),
            (Kind::Home, true) => Some("home"),
            (Kind::Work, false) => Some("WORK"),
            (Kind::Work, true) => Some("work"),
            (Kind::Other, _) => None,
        }
    }
}

/// A phone number or email address with its kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub kind: Kind,
    pub value: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Address {
    pub kind: Option<Kind>,
    pub street: String,
    pub postcode: String,
    pub city: String,
    pub region: String,
    pub country: String,
}

impl Address {
    pub fn is_empty(&self) -> bool {
        [&self.street, &self.postcode, &self.city, &self.region, &self.country].iter().all(|s| s.trim().is_empty())
    }

    /// One line for a map search or a list.
    pub fn one_line(&self) -> String {
        let city = [self.postcode.trim(), self.city.trim()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(" ");
        [self.street.trim(), &city, self.region.trim(), self.country.trim()]
            .iter()
            .filter(|s| !s.is_empty())
            .copied()
            .collect::<Vec<_>>()
            .join(", ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Birthday {
    Full(NaiveDate),
    /// `--MMDD`: the day without a year.
    NoYear {
        month: u32,
        day: u32,
    },
}

/// A photo as written: inline bytes still encoded, or a link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Photo {
    /// Base64 data and its media type (`image/jpeg`).
    Inline {
        mime: String,
        base64: String,
    },
    Uri(String),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardView {
    pub uid: Option<String>,
    /// FN: the name shown.
    pub name: String,
    pub given: String,
    pub family: String,
    pub org: String,
    pub phones: Vec<Typed>,
    pub emails: Vec<Typed>,
    pub addresses: Vec<Address>,
    pub birthday: Option<Birthday>,
    pub note: String,
    pub photo: Option<Photo>,
    pub v4: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CardError {
    #[error("not a contact")]
    NotACard,
    #[error("a contact needs a name or an organization")]
    NoName,
}

/// vCard TEXT escaping (the same rules as iCalendar).
pub fn escape(s: &str) -> String {
    crate::edit::escape_text(s)
}

fn unescape(s: &str) -> String {
    crate::object::unescape(s)
}

/// Split a structured value on unescaped `;`, each part unescaped.
fn parts(v: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut esc = false;
    for c in v.chars() {
        if esc {
            cur.push('\\');
            cur.push(c);
            esc = false;
        } else if c == '\\' {
            esc = true;
        } else if c == ';' {
            out.push(unescape(&cur));
            cur.clear();
        } else {
            cur.push(c);
        }
    }
    if esc {
        cur.push('\\');
    }
    out.push(unescape(&cur));
    out
}

fn phone_value(line: &Line) -> String {
    let v = unescape(line.value());
    v.strip_prefix("tel:").map(str::to_string).unwrap_or(v)
}

fn email_value(line: &Line) -> String {
    let v = unescape(line.value());
    v.strip_prefix("mailto:").map(str::to_string).unwrap_or(v)
}

fn address_of(line: &Line) -> Address {
    let p = parts(line.value());
    let get = |i: usize| p.get(i).cloned().unwrap_or_default();
    // PO box; extended; street; locality; region; postal code; country.
    let street = [get(0), get(1), get(2)].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", ");
    let kind = Kind::of(line);
    Address {
        kind: Some(kind).filter(|k| *k != Kind::Other),
        street,
        city: get(3),
        region: get(4),
        postcode: get(5),
        country: get(6),
    }
}

fn birthday_of(v: &str) -> Option<Birthday> {
    let v = v.trim();
    let v = v.split('T').next().unwrap_or(v);
    if let Some(rest) = v.strip_prefix("--") {
        let d: String = rest.chars().filter(char::is_ascii_digit).collect();
        if d.len() == 4 {
            let (m, day) = (d[..2].parse().ok()?, d[2..].parse().ok()?);
            NaiveDate::from_ymd_opt(2000, m, day)?;
            return Some(Birthday::NoYear { month: m, day });
        }
        return None;
    }
    let d: String = v.chars().filter(char::is_ascii_digit).collect();
    (d.len() == 8).then(|| NaiveDate::parse_from_str(&d, "%Y%m%d").ok().map(Birthday::Full)).flatten()
}

fn photo_of(line: &Line) -> Option<Photo> {
    let v = line.value().trim();
    if let Some(rest) = v.strip_prefix("data:") {
        let (head, data) = rest.split_once(',')?;
        let mime = head.trim_end_matches(";base64").to_ascii_lowercase();
        return head.ends_with(";base64").then(|| Photo::Inline { mime, base64: data.to_string() });
    }
    let enc = line.param("ENCODING").map(|e| e.to_ascii_lowercase());
    if matches!(enc.as_deref(), Some("b") | Some("base64")) {
        let t = line.param("TYPE").unwrap_or_else(|| "jpeg".into()).to_ascii_lowercase();
        let mime = if t.contains('/') { t } else { format!("image/{t}") };
        return Some(Photo::Inline { mime, base64: v.split_whitespace().collect() });
    }
    (!v.is_empty()).then(|| Photo::Uri(v.to_string()))
}

fn card_of(doc: &Document) -> Result<&Component, CardError> {
    match doc.components.as_slice() {
        [c] if c.name == "VCARD" => Ok(c),
        _ => Err(CardError::NotACard),
    }
}

fn all<'a>(c: &'a Component, name: &'a str) -> impl Iterator<Item = &'a Line> {
    c.properties().filter(move |l| l.name() == name)
}

/// The fields people edit.
pub fn read(text: &str) -> Result<CardView, CardError> {
    let doc = Document::parse(text).map_err(|_| CardError::NotACard)?;
    let c = card_of(&doc)?;
    let first = |n: &str| c.property(n).map(|l| unescape(l.value())).unwrap_or_default();
    let n = c.property("N").map(|l| parts(l.value())).unwrap_or_default();
    let org = c
        .property("ORG")
        .map(|l| parts(l.value()).into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "))
        .unwrap_or_default();
    Ok(CardView {
        uid: c.property("UID").map(|l| l.value().to_string()).filter(|u| !u.is_empty()),
        name: first("FN"),
        family: n.first().cloned().unwrap_or_default(),
        given: n.get(1).cloned().unwrap_or_default(),
        org,
        phones: all(c, "TEL").map(|l| Typed { kind: Kind::of(l), value: phone_value(l) }).collect(),
        emails: all(c, "EMAIL").map(|l| Typed { kind: Kind::of(l), value: email_value(l) }).collect(),
        addresses: all(c, "ADR").map(address_of).collect(),
        birthday: c.property("BDAY").and_then(|l| birthday_of(l.value())),
        note: first("NOTE"),
        photo: c.property("PHOTO").and_then(photo_of),
        v4: c.property("VERSION").is_some_and(|l| l.value().trim() == "4.0"),
    })
}

/// What to change; `None` leaves a field alone. Lists replace the whole
/// set (unchanged entries keep their lines).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardChange {
    pub given: Option<String>,
    pub family: Option<String>,
    pub org: Option<String>,
    pub phones: Option<Vec<Typed>>,
    pub emails: Option<Vec<Typed>>,
    pub addresses: Option<Vec<Address>>,
    /// `Some(None)` removes it.
    pub birthday: Option<Option<NaiveDate>>,
    pub note: Option<String>,
}

fn insert_at(c: &Component, name: &str) -> usize {
    // After the last line of this name, else before the end.
    c.items
        .iter()
        .rposition(|i| matches!(i, Item::Property(l) if l.name() == name))
        .map(|p| p + 1)
        .unwrap_or(c.items.len())
}

fn set_single(c: &mut Component, name: &str, line: Option<Line>) {
    let at = c.items.iter().position(|i| matches!(i, Item::Property(l) if l.name() == name));
    match (at, line) {
        (Some(i), Some(l)) => c.items[i] = Item::Property(l),
        (Some(i), None) => {
            c.items.remove(i);
        }
        (None, Some(l)) => {
            let i = insert_at(c, name);
            c.items.insert(i, Item::Property(l));
        }
        (None, None) => {}
    }
}

/// Keep lines still wanted, drop the rest, add the new ones.
fn replace_list<T: PartialEq>(
    c: &mut Component,
    name: &str,
    want: &[T],
    read: impl Fn(&Line) -> T,
    write: impl Fn(&T) -> Line,
) {
    let mut left: Vec<&T> = want.iter().collect();
    c.items.retain(|i| match i {
        Item::Property(l) if l.name() == name => {
            let v = read(l);
            match left.iter().position(|w| **w == v) {
                Some(p) => {
                    left.remove(p);
                    true
                }
                None => false,
            }
        }
        _ => true,
    });
    let mut at = insert_at(c, name);
    if !c.items.iter().any(|i| matches!(i, Item::Property(l) if l.name() == name)) {
        // No lines of this kind left: after the name lines, or at the end.
        at = ["ORG", "N", "FN"]
            .iter()
            .find_map(|n| c.items.iter().rposition(|i| matches!(i, Item::Property(l) if l.name() == *n)))
            .map_or(c.items.len(), |p| p + 1);
    }
    for (k, w) in left.into_iter().enumerate() {
        c.items.insert(at + k, Item::Property(write(w)));
    }
}

fn typed_line(name: &str, t: &Typed, v4: bool, phone: bool) -> Line {
    let ty = t.kind.param(v4, phone).map(|p| format!(";TYPE={p}")).unwrap_or_default();
    Line::new(format!("{name}{ty}:{}", escape(t.value.trim())))
}

fn address_line(a: &Address, v4: bool) -> Line {
    let ty = a.kind.and_then(|k| k.param(v4, false)).map(|p| format!(";TYPE={p}")).unwrap_or_default();
    let f = |s: &str| escape(s.trim());
    Line::new(format!(
        "ADR{ty}:;;{};{};{};{};{}",
        f(&a.street),
        f(&a.city),
        f(&a.region),
        f(&a.postcode),
        f(&a.country)
    ))
}

fn display_name(given: &str, family: &str, org: &str) -> String {
    let n = format!("{} {}", given.trim(), family.trim()).trim().to_string();
    if n.is_empty() { org.trim().to_string() } else { n }
}

fn rev(now: NaiveDateTime) -> Line {
    Line::new(format!("REV:{}Z", now.format("%Y%m%dT%H%M%S")))
}

/// Apply a change; `now` (UTC) goes into `REV` when anything changed.
pub fn apply(text: &str, change: &CardChange, now: NaiveDateTime) -> Result<String, CardError> {
    let mut doc = Document::parse(text).map_err(|_| CardError::NotACard)?;
    let before = read(text)?;
    let v4 = before.v4;
    {
        let c = doc.components.first_mut().filter(|c| c.name == "VCARD").ok_or(CardError::NotACard)?;
        let given = change.given.clone().unwrap_or_else(|| before.given.clone());
        let family = change.family.clone().unwrap_or_else(|| before.family.clone());
        let org = change.org.clone().unwrap_or_else(|| before.org.clone());
        // Changed only when the edit says so (a stray space in a stored name
        // is not a change).
        let names_changed = change.given.as_ref().is_some_and(|g| g.trim() != before.given.trim())
            || change.family.as_ref().is_some_and(|f| f.trim() != before.family.trim());
        let org_changed = change.org.as_ref().is_some_and(|o| o.trim() != before.org.trim());
        // The shown name follows the names when it was derived from them;
        // one someone chose stays. Some shown name must remain.
        let old_derived = display_name(&before.given, &before.family, &before.org);
        let fn_follows = (names_changed || org_changed) && (before.name.is_empty() || before.name == old_derived);
        let new_fn = if fn_follows { display_name(&given, &family, &org) } else { before.name.clone() };
        if new_fn.trim().is_empty() {
            return Err(CardError::NoName);
        }
        if names_changed {
            // Keep the other N parts (middle names, prefixes, suffixes).
            let mut n = c.property("N").map(|l| parts(l.value())).unwrap_or_default();
            n.resize(n.len().max(5), String::new());
            n[0] = family.trim().to_string();
            n[1] = given.trim().to_string();
            let joined: Vec<String> = n.iter().map(|p| escape(p)).collect();
            set_single(c, "N", Some(Line::new(format!("N:{}", joined.join(";")))));
        }
        if fn_follows {
            set_single(c, "FN", Some(Line::new(format!("FN:{}", escape(&new_fn)))));
        }
        if let Some(o) = &change.org
            && org_changed
        {
            set_single(c, "ORG", (!o.trim().is_empty()).then(|| Line::new(format!("ORG:{}", escape(o.trim())))));
        }
        if let Some(phones) = &change.phones {
            let want: Vec<Typed> = phones
                .iter()
                .filter(|t| !t.value.trim().is_empty())
                .map(|t| Typed { kind: t.kind, value: t.value.trim().to_string() })
                .collect();
            replace_list(
                c,
                "TEL",
                &want,
                |l| Typed { kind: Kind::of(l), value: phone_value(l) },
                |t| typed_line("TEL", t, v4, true),
            );
        }
        if let Some(emails) = &change.emails {
            let want: Vec<Typed> = emails
                .iter()
                .filter(|t| !t.value.trim().is_empty())
                .map(|t| Typed { kind: t.kind, value: t.value.trim().to_string() })
                .collect();
            replace_list(
                c,
                "EMAIL",
                &want,
                |l| Typed { kind: Kind::of(l), value: email_value(l) },
                |t| typed_line("EMAIL", t, v4, false),
            );
        }
        if let Some(addrs) = &change.addresses {
            let want: Vec<Address> = addrs.iter().filter(|a| !a.is_empty()).cloned().collect();
            replace_list(c, "ADR", &want, address_of, |a| address_line(a, v4));
        }
        if let Some(b) = &change.birthday {
            let changed = match (b, before.birthday) {
                (None, None) => false,
                (Some(d), Some(Birthday::Full(x))) => *d != x,
                _ => true,
            };
            if changed {
                let line = b
                    .map(|d| Line::new(format!("BDAY:{}", if v4 { d.format("%Y%m%d") } else { d.format("%Y-%m-%d") })));
                set_single(c, "BDAY", line);
            }
        }
        if let Some(n) = &change.note
            && n.trim() != before.note.trim()
        {
            set_single(c, "NOTE", (!n.trim().is_empty()).then(|| Line::new(format!("NOTE:{}", escape(n.trim())))));
        }
    }
    let out = doc.to_string();
    if out == text {
        return Ok(out);
    }
    let c = doc.components.first_mut().expect("checked above");
    set_single(c, "REV", Some(rev(now)));
    Ok(doc.to_string())
}

/// A new vCard 3.0, the version the DAV door advertises.
pub fn new_card(change: &CardChange, uid: &str, now: NaiveDateTime) -> Result<String, CardError> {
    let given = change.given.clone().unwrap_or_default();
    let family = change.family.clone().unwrap_or_default();
    let org = change.org.clone().unwrap_or_default();
    let fname = display_name(&given, &family, &org);
    if fname.is_empty() {
        return Err(CardError::NoName);
    }
    let skeleton = format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//CalZone//CalZone//EN\r\nUID:{uid}\r\nFN:{}\r\nN:{};{};;;\r\nEND:VCARD\r\n",
        escape(&fname),
        escape(family.trim()),
        escape(given.trim())
    );
    let rest = CardChange { given: None, family: None, ..change.clone() };
    apply(&skeleton, &rest, now)
}

/// A birthday for display: day and month, plus the year when known.
pub fn birthday_parts(b: Birthday) -> (u32, u32, Option<i32>) {
    match b {
        Birthday::Full(d) => (d.day(), d.month(), Some(d.year())),
        Birthday::NoYear { month, day } => (day, month, None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 6).unwrap().and_hms_opt(12, 0, 0).unwrap()
    }

    const V3: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;Augusta;Lady;\r\nORG:Analytical\\, Ltd\r\n\
TEL;TYPE=CELL,VOICE:+44 7700 900001\r\nTEL;TYPE=HOME:+44 20 7946 0001\r\nitem1.EMAIL;TYPE=INTERNET:ada@example.org\r\nitem1.X-ABLabel:Lab\r\n\
ADR;TYPE=HOME:;;12 St James\\, Sq;London;;SW1Y 4JH;UK\r\nBDAY:1815-12-10\r\nNOTE:Poet\\nof science\r\nX-CUSTOM;FOO=bar:keep\r\n\
PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQ\r\n SkZJRg==\r\nEND:VCARD\r\n";

    fn lines(s: &str) -> Vec<String> {
        s.split("\r\n").map(str::to_string).collect()
    }

    #[test]
    fn reading() {
        let v = read(V3).unwrap();
        assert_eq!(
            (v.name.as_str(), v.given.as_str(), v.family.as_str(), v.org.as_str()),
            ("Ada Lovelace", "Ada", "Lovelace", "Analytical, Ltd")
        );
        assert_eq!(
            v.phones,
            vec![
                Typed { kind: Kind::Mobile, value: "+44 7700 900001".into() },
                Typed { kind: Kind::Home, value: "+44 20 7946 0001".into() }
            ]
        );
        assert_eq!(v.emails, vec![Typed { kind: Kind::Other, value: "ada@example.org".into() }]);
        assert_eq!(v.addresses[0].street, "12 St James, Sq");
        assert_eq!(
            (v.addresses[0].city.as_str(), v.addresses[0].postcode.as_str(), v.addresses[0].kind),
            ("London", "SW1Y 4JH", Some(Kind::Home))
        );
        assert_eq!(v.addresses[0].one_line(), "12 St James, Sq, SW1Y 4JH London, UK");
        assert_eq!(v.birthday, Some(Birthday::Full(NaiveDate::from_ymd_opt(1815, 12, 10).unwrap())));
        assert_eq!(v.note, "Poet\nof science");
        assert_eq!(v.photo, Some(Photo::Inline { mime: "image/jpeg".into(), base64: "/9j/4AAQSkZJRg==".into() }));
        let v4 = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nTEL;VALUE=uri;TYPE=\"voice,cell\":tel:+31-6-00000000\r\nBDAY:--0315\r\nPHOTO:data:image/png;base64,iVBOR\r\nEND:VCARD\r\n";
        let v = read(v4).unwrap();
        assert!(v.v4);
        assert_eq!(v.phones[0], Typed { kind: Kind::Mobile, value: "+31-6-00000000".into() });
        assert_eq!(v.birthday, Some(Birthday::NoYear { month: 3, day: 15 }));
        assert_eq!(v.photo, Some(Photo::Inline { mime: "image/png".into(), base64: "iVBOR".into() }));
        assert_eq!(read("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n"), Err(CardError::NotACard));
    }

    #[test]
    fn edits_touch_only_what_changed() {
        let none = apply(V3, &CardChange::default(), now()).unwrap();
        assert_eq!(none, V3, "nothing changed, not even REV");
        // Same values back: still nothing.
        let v = read(V3).unwrap();
        let same = CardChange {
            phones: Some(v.phones.clone()),
            emails: Some(v.emails.clone()),
            addresses: Some(v.addresses.clone()),
            note: Some(v.note.clone()),
            ..Default::default()
        };
        assert_eq!(apply(V3, &same, now()).unwrap(), V3);
        // One phone removed, one added, one kept.
        let change = CardChange {
            phones: Some(vec![
                Typed { kind: Kind::Mobile, value: "+44 7700 900001".into() },
                Typed { kind: Kind::Work, value: "+44 20 0000 0002".into() },
            ]),
            ..Default::default()
        };
        let out = apply(V3, &change, now()).unwrap();
        let (b, a) = (lines(V3), lines(&out));
        assert!(a.contains(&"TEL;TYPE=CELL,VOICE:+44 7700 900001".to_string()), "kept byte for byte");
        assert!(!out.contains("7946"));
        assert!(a.contains(&"TEL;TYPE=WORK:+44 20 0000 0002".to_string()));
        assert!(a.contains(&"REV:20261106T120000Z".to_string()));
        for keep in [
            "X-CUSTOM;FOO=bar:keep",
            "item1.X-ABLabel:Lab",
            "PHOTO;ENCODING=b;TYPE=JPEG:/9j/4AAQ",
            " SkZJRg==",
            "N:Lovelace;Ada;Augusta;Lady;",
        ] {
            assert!(b.contains(&keep.to_string()) && a.contains(&keep.to_string()), "{keep}");
        }
        // The new phone sits with the other phones.
        let tel = a.iter().position(|l| l.starts_with("TEL;TYPE=CELL")).unwrap();
        assert!(a[tel + 1].starts_with("TEL;TYPE=WORK"));
    }

    #[test]
    fn names_keep_their_other_parts_and_fn_follows() {
        let out = apply(V3, &CardChange { family: Some("King".into()), ..Default::default() }, now()).unwrap();
        assert!(out.contains("N:King;Ada;Augusta;Lady;\r\n") && out.contains("FN:Ada King\r\n"));
        // A shown name someone chose (not derived) is left alone.
        let chosen = V3.replace("FN:Ada Lovelace", "FN:Countess of Lovelace");
        let out =
            apply(&chosen, &CardChange { given: Some("Augusta Ada".into()), ..Default::default() }, now()).unwrap();
        assert!(out.contains("FN:Countess of Lovelace") && out.contains("N:Lovelace;Augusta Ada;"));
        assert_eq!(
            apply(
                V3,
                &CardChange {
                    given: Some("".into()),
                    family: Some(" ".into()),
                    org: Some("".into()),
                    ..Default::default()
                },
                now()
            ),
            Err(CardError::NoName)
        );
    }

    #[test]
    fn birthdays_notes_and_addresses() {
        let out = apply(
            V3,
            &CardChange { birthday: Some(None), note: Some("".into()), addresses: Some(vec![]), ..Default::default() },
            now(),
        )
        .unwrap();
        assert!(!out.contains("BDAY") && !out.contains("NOTE") && !out.contains("ADR"));
        let addr = Address {
            kind: Some(Kind::Work),
            street: "Main st 1".into(),
            postcode: "1011 AB".into(),
            city: "Amsterdam".into(),
            country: "NL".into(),
            ..Default::default()
        };
        let out = apply(
            V3,
            &CardChange {
                addresses: Some(vec![addr.clone()]),
                birthday: Some(NaiveDate::from_ymd_opt(1815, 12, 11)),
                ..Default::default()
            },
            now(),
        )
        .unwrap();
        assert!(out.contains("ADR;TYPE=WORK:;;Main st 1;Amsterdam;;1011 AB;NL") && out.contains("BDAY:1815-12-11"));
        assert_eq!(read(&out).unwrap().addresses, vec![addr]);
        // A birthday without a year stays when the form leaves it alone, and
        // goes when it is cleared on purpose.
        let v4 = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nBDAY:--0315\r\nEND:VCARD\r\n";
        assert!(
            apply(v4, &CardChange { note: Some("n".into()), ..Default::default() }, now())
                .unwrap()
                .contains("BDAY:--0315")
        );
        assert!(
            !apply(v4, &CardChange { birthday: Some(None), ..Default::default() }, now()).unwrap().contains("BDAY")
        );
        assert!(
            apply(
                v4,
                &CardChange { birthday: Some(NaiveDate::from_ymd_opt(1990, 3, 15)), ..Default::default() },
                now()
            )
            .unwrap()
            .contains("BDAY:19900315"),
            "v4 spelling"
        );
    }

    #[test]
    fn new_cards_are_vcard_3() {
        let change = CardChange {
            given: Some("Bea".into()),
            family: Some("Smith; Jr".into()),
            phones: Some(vec![
                Typed { kind: Kind::Mobile, value: "+31 6 1".into() },
                Typed { kind: Kind::Home, value: " ".into() },
            ]),
            emails: Some(vec![Typed { kind: Kind::Work, value: "bea@example.org".into() }]),
            ..Default::default()
        };
        let text = new_card(&change, "u1", now()).unwrap();
        assert!(text.starts_with("BEGIN:VCARD\r\nVERSION:3.0\r\n") && text.contains("UID:u1"));
        assert!(text.contains("FN:Bea Smith\\; Jr") && text.contains("N:Smith\\; Jr;Bea;;;"));
        assert!(text.contains("TEL;TYPE=CELL:+31 6 1") && text.contains("EMAIL;TYPE=WORK:bea@example.org"));
        assert_eq!(text.matches("TEL").count(), 1, "empty rows are dropped");
        let v = read(&text).unwrap();
        assert_eq!((v.family.as_str(), v.name.as_str()), ("Smith; Jr", "Bea Smith; Jr"));
        crate::object::inspect_card(&Document::parse(&text).unwrap()).unwrap();
        let org_only =
            new_card(&CardChange { org: Some("Dentist".into()), ..Default::default() }, "u2", now()).unwrap();
        assert!(org_only.contains("FN:Dentist") && org_only.contains("ORG:Dentist"));
        assert_eq!(new_card(&CardChange::default(), "u3", now()), Err(CardError::NoName));
    }
}
