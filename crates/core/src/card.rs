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

/// A wanted label: `None` takes any, `Some("")` only a line without one.
fn same_label(want: Option<&str>, line: Option<&str>) -> bool {
    want.is_none_or(|w| w.trim() == line.unwrap_or("").trim())
}

/// A phone number or email address with its kind, and the custom label
/// some apps give it (`item1.TEL` with `item1.X-ABLabel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Typed {
    pub kind: Kind,
    pub value: String,
    /// In a change, `None` leaves a line's label as it is and `Some("")`
    /// asks for a line without one.
    pub label: Option<String>,
}

impl Typed {
    pub fn new(kind: Kind, value: impl Into<String>) -> Self {
        Self { kind, value: value.into(), label: None }
    }

    /// A wanted entry matches a line with its kind and value; a label only
    /// counts when the change gives one.
    fn wants(&self, line: &Typed) -> bool {
        self.kind == line.kind && self.value == line.value && same_label(self.label.as_deref(), line.label.as_deref())
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Address {
    pub kind: Option<Kind>,
    pub street: String,
    pub postcode: String,
    pub city: String,
    pub region: String,
    pub country: String,
    /// As for [`Typed`].
    pub label: Option<String>,
}

impl Address {
    fn wants(&self, line: &Address) -> bool {
        let strip = |a: &Address| Address { label: None, ..a.clone() };
        strip(self) == strip(line) && same_label(self.label.as_deref(), line.label.as_deref())
    }

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
    /// N's other parts: additional names, prefixes, suffixes.
    pub middle: String,
    pub prefix: String,
    pub suffix: String,
    pub org: String,
    /// TITLE: the job title.
    pub title: String,
    pub phones: Vec<Typed>,
    pub emails: Vec<Typed>,
    pub addresses: Vec<Address>,
    pub birthday: Option<Birthday>,
    pub note: String,
    pub photo: Option<Photo>,
    /// CATEGORIES: the groups (labels) it is in.
    pub categories: Vec<String>,
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
    split_escaped(v, ';')
}

/// Split on an unescaped `sep`, each part unescaped.
fn split_escaped(v: &str, sep: char) -> Vec<String> {
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
        } else if c == sep {
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

/// The custom label of a grouped line (`item1.X-ABLabel`), if any.
fn label_of(c: &Component, line: &Line) -> Option<String> {
    let g = line.group()?;
    c.properties()
        .find(|l| l.name() == "X-ABLABEL" && l.group().is_some_and(|x| x.eq_ignore_ascii_case(g)))
        .map(|l| unescape(l.value()).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn phone_of(c: &Component, line: &Line) -> Typed {
    Typed { kind: Kind::of(line), value: phone_value(line), label: label_of(c, line) }
}

fn email_of(c: &Component, line: &Line) -> Typed {
    Typed { kind: Kind::of(line), value: email_value(line), label: label_of(c, line) }
}

fn address_of(c: &Component, line: &Line) -> Address {
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
        label: label_of(c, line),
    }
}

fn categories_of(c: &Component) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for l in all(c, "CATEGORIES") {
        for v in split_escaped(l.value(), ',') {
            let v = v.trim().to_string();
            if !v.is_empty() && !out.contains(&v) {
                out.push(v);
            }
        }
    }
    out
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
        // Some writers escape the comma as in a TEXT value.
        let rest = rest.replacen("\\,", ",", 1);
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
        middle: n.get(2).cloned().unwrap_or_default(),
        prefix: n.get(3).cloned().unwrap_or_default(),
        suffix: n.get(4).cloned().unwrap_or_default(),
        org,
        title: first("TITLE"),
        phones: all(c, "TEL").map(|l| phone_of(c, l)).collect(),
        emails: all(c, "EMAIL").map(|l| email_of(c, l)).collect(),
        addresses: all(c, "ADR").map(|l| address_of(c, l)).collect(),
        birthday: c.property("BDAY").and_then(|l| birthday_of(l.value())),
        note: first("NOTE"),
        photo: c.property("PHOTO").and_then(photo_of),
        categories: categories_of(c),
        v4: c.property("VERSION").is_some_and(|l| l.value().trim() == "4.0"),
    })
}

/// What to change; `None` leaves a field alone. Lists replace the whole
/// set (unchanged entries keep their lines).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CardChange {
    /// FN, when someone chose the name shown; otherwise it follows the
    /// names it was made from.
    pub name: Option<String>,
    pub given: Option<String>,
    pub family: Option<String>,
    pub middle: Option<String>,
    pub prefix: Option<String>,
    pub suffix: Option<String>,
    pub org: Option<String>,
    pub title: Option<String>,
    pub phones: Option<Vec<Typed>>,
    pub emails: Option<Vec<Typed>>,
    pub addresses: Option<Vec<Address>>,
    /// `Some(None)` removes it.
    pub birthday: Option<Option<Birthday>>,
    pub note: Option<String>,
    /// The groups, as a set.
    pub categories: Option<Vec<String>>,
    /// `Some(None)` removes it.
    pub photo: Option<Option<Photo>>,
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

/// Every line named `name` replaced by `line` (at the first one's place).
fn set_all(c: &mut Component, name: &str, line: Option<Line>) {
    let at = c.items.iter().position(|i| matches!(i, Item::Property(l) if l.name() == name));
    c.items.retain(|i| !matches!(i, Item::Property(l) if l.name() == name));
    if let Some(l) = line {
        let at = at.unwrap_or_else(|| insert_at(c, name));
        c.items.insert(at, Item::Property(l));
    }
}

/// The next free `itemN` group.
fn next_group(c: &Component) -> u32 {
    c.properties()
        .filter_map(|l| l.group().map(str::to_ascii_lowercase))
        .filter_map(|g| g.strip_prefix("item").and_then(|n| n.parse::<u32>().ok()))
        .max()
        .map_or(1, |n| n + 1)
}

/// Keep lines still wanted, drop the rest (with the label of a grouped
/// line), add the new ones.
fn replace_list<T>(
    c: &mut Component,
    name: &str,
    want: &[T],
    read: impl Fn(&Component, &Line) -> T,
    wants: impl Fn(&T, &T) -> bool,
    mut write: impl FnMut(&T) -> Vec<Line>,
) {
    let values: Vec<Option<T>> = c
        .items
        .iter()
        .map(|i| match i {
            Item::Property(l) if l.name() == name => Some(read(c, l)),
            _ => None,
        })
        .collect();
    let mut left: Vec<&T> = want.iter().collect();
    let mut keep = vec![true; c.items.len()];
    let mut dropped: Vec<String> = Vec::new();
    for (i, v) in values.iter().enumerate() {
        let Some(v) = v else { continue };
        match left.iter().position(|w| wants(w, v)) {
            Some(p) => {
                left.remove(p);
            }
            None => {
                keep[i] = false;
                if let Item::Property(l) = &c.items[i]
                    && let Some(g) = l.group()
                {
                    dropped.push(g.to_ascii_lowercase());
                }
            }
        }
    }
    // A dropped line's label goes too, unless its group still holds a line.
    let group_of = |i: &Item| match i {
        Item::Property(l) => l.group().map(|g| (g.to_ascii_lowercase(), l.name() == "X-ABLABEL")),
        _ => None,
    };
    for g in &dropped {
        let used =
            c.items.iter().zip(&keep).any(|(i, k)| *k && group_of(i).is_some_and(|(x, label)| x == *g && !label));
        if !used {
            for (i, item) in c.items.iter().enumerate() {
                if group_of(item).is_some_and(|(x, label)| x == *g && label) {
                    keep[i] = false;
                }
            }
        }
    }
    let mut k = keep.into_iter();
    c.items.retain(|_| k.next().unwrap_or(true));
    let mut at = insert_at(c, name);
    if !c.items.iter().any(|i| matches!(i, Item::Property(l) if l.name() == name)) {
        // No lines of this kind left: after the name lines, or at the end.
        at = ["ORG", "N", "FN"]
            .iter()
            .find_map(|n| c.items.iter().rposition(|i| matches!(i, Item::Property(l) if l.name() == *n)))
            .map_or(c.items.len(), |p| p + 1);
    }
    for w in left {
        for line in write(w) {
            c.items.insert(at, Item::Property(line));
            at += 1;
        }
    }
}

/// `name` + `params` + `value`, grouped with an `X-ABLabel` when labeled.
fn labeled(name: &str, params: &str, value: &str, label: Option<&str>, next: &mut u32) -> Vec<Line> {
    match label.map(str::trim).filter(|l| !l.is_empty()) {
        None => vec![Line::new(format!("{name}{params}:{value}"))],
        Some(label) => {
            let g = format!("item{next}");
            *next += 1;
            vec![
                Line::new(format!("{g}.{name}{params}:{value}")),
                Line::new(format!("{g}.X-ABLabel:{}", escape(label))),
            ]
        }
    }
}

fn typed_lines(name: &str, t: &Typed, v4: bool, phone: bool, next: &mut u32) -> Vec<Line> {
    let ty = t.kind.param(v4, phone).map(|p| format!(";TYPE={p}")).unwrap_or_default();
    labeled(name, &ty, &escape(t.value.trim()), t.label.as_deref(), next)
}

fn address_lines(a: &Address, v4: bool, next: &mut u32) -> Vec<Line> {
    let ty = a.kind.and_then(|k| k.param(v4, false)).map(|p| format!(";TYPE={p}")).unwrap_or_default();
    let f = |s: &str| escape(s.trim());
    let value = format!(";;{};{};{};{};{}", f(&a.street), f(&a.city), f(&a.region), f(&a.postcode), f(&a.country));
    labeled("ADR", &ty, &value, a.label.as_deref(), next)
}

fn display_name(given: &str, family: &str, org: &str) -> String {
    let n = format!("{} {}", given.trim(), family.trim()).trim().to_string();
    if n.is_empty() { org.trim().to_string() } else { n }
}

/// Every name part, the way phones show a name.
fn full_name(prefix: &str, given: &str, middle: &str, family: &str, suffix: &str, org: &str) -> String {
    let n = [prefix, given, middle, family, suffix]
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if n.is_empty() { org.trim().to_string() } else { n }
}

fn birthday_value(b: Birthday, v4: bool) -> String {
    match (b, v4) {
        (Birthday::Full(d), true) => d.format("%Y%m%d").to_string(),
        (Birthday::Full(d), false) => d.format("%Y-%m-%d").to_string(),
        (Birthday::NoYear { month, day }, true) => format!("--{month:02}{day:02}"),
        (Birthday::NoYear { month, day }, false) => format!("--{month:02}-{day:02}"),
    }
}

fn photo_line(p: &Photo, v4: bool) -> Line {
    match (p, v4) {
        (Photo::Inline { mime, base64 }, true) => Line::new(format!("PHOTO:data:{mime};base64,{base64}")),
        (Photo::Inline { mime, base64 }, false) => {
            let t = mime.rsplit('/').next().unwrap_or("jpeg").to_ascii_uppercase();
            Line::new(format!("PHOTO;ENCODING=b;TYPE={t}:{base64}"))
        }
        (Photo::Uri(u), true) => Line::new(format!("PHOTO:{u}")),
        (Photo::Uri(u), false) => Line::new(format!("PHOTO;VALUE=uri:{u}")),
    }
}

/// The same picture: inline data compared without whitespace.
fn same_photo(a: Option<&Photo>, b: Option<&Photo>) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    match (a, b) {
        (None, None) => true,
        (Some(Photo::Inline { base64: x, .. }), Some(Photo::Inline { base64: y, .. })) => squash(x) == squash(y),
        (Some(Photo::Uri(x)), Some(Photo::Uri(y))) => x.trim() == y.trim(),
        _ => false,
    }
}

/// A set of names: trimmed, without blanks or repeats, in their order.
fn name_set(v: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for x in v.iter().map(|x| x.trim()).filter(|x| !x.is_empty()) {
        if !out.iter().any(|y| y == x) {
            out.push(x.to_string());
        }
    }
    out
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
        let pick = |x: &Option<String>, b: &str| x.as_ref().map_or_else(|| b.to_string(), |s| s.trim().to_string());
        let given = pick(&change.given, &before.given);
        let family = pick(&change.family, &before.family);
        let middle = pick(&change.middle, &before.middle);
        let prefix = pick(&change.prefix, &before.prefix);
        let suffix = pick(&change.suffix, &before.suffix);
        let org = pick(&change.org, &before.org);
        // Changed only when the edit says so (a stray space in a stored name
        // is not a change).
        let differs = |x: &Option<String>, b: &str| x.as_ref().is_some_and(|s| s.trim() != b.trim());
        let names_changed = differs(&change.given, &before.given)
            || differs(&change.family, &before.family)
            || differs(&change.middle, &before.middle)
            || differs(&change.prefix, &before.prefix)
            || differs(&change.suffix, &before.suffix);
        let org_changed = differs(&change.org, &before.org);
        // The shown name follows the names when it was made from them (the
        // short form or every part); one someone chose stays, unless the
        // change chooses another. Some shown name must remain.
        let short = display_name(&before.given, &before.family, &before.org);
        let full =
            full_name(&before.prefix, &before.given, &before.middle, &before.family, &before.suffix, &before.org);
        let derived = before.name.is_empty() || before.name == short || before.name == full;
        let chosen = change.name.as_deref().map(str::trim).filter(|n| !n.is_empty() && *n != before.name.trim());
        let new_fn = match chosen {
            Some(n) => n.to_string(),
            None if (names_changed || org_changed) && derived => {
                if before.name == full && full != short {
                    full_name(&prefix, &given, &middle, &family, &suffix, &org)
                } else {
                    display_name(&given, &family, &org)
                }
            }
            None => before.name.clone(),
        };
        if new_fn.trim().is_empty() {
            return Err(CardError::NoName);
        }
        if names_changed {
            // Only the parts the change gives; the rest stay.
            let mut n = c.property("N").map(|l| parts(l.value())).unwrap_or_default();
            n.resize(n.len().max(5), String::new());
            for (i, v) in
                [&change.family, &change.given, &change.middle, &change.prefix, &change.suffix].into_iter().enumerate()
            {
                if let Some(v) = v {
                    n[i] = v.trim().to_string();
                }
            }
            let joined: Vec<String> = n.iter().map(|p| escape(p)).collect();
            set_single(c, "N", Some(Line::new(format!("N:{}", joined.join(";")))));
        }
        if new_fn != before.name {
            set_single(c, "FN", Some(Line::new(format!("FN:{}", escape(&new_fn)))));
        }
        if let Some(o) = &change.org
            && org_changed
        {
            set_single(c, "ORG", (!o.trim().is_empty()).then(|| Line::new(format!("ORG:{}", escape(o.trim())))));
        }
        if let Some(t) = &change.title
            && differs(&change.title, &before.title)
        {
            set_single(c, "TITLE", (!t.trim().is_empty()).then(|| Line::new(format!("TITLE:{}", escape(t.trim())))));
        }
        let mut next = next_group(c);
        let trimmed = |list: &[Typed]| -> Vec<Typed> {
            list.iter()
                .filter(|t| !t.value.trim().is_empty())
                .map(|t| Typed { value: t.value.trim().to_string(), ..t.clone() })
                .collect()
        };
        if let Some(phones) = &change.phones {
            replace_list(c, "TEL", &trimmed(phones), phone_of, Typed::wants, |t| {
                typed_lines("TEL", t, v4, true, &mut next)
            });
        }
        if let Some(emails) = &change.emails {
            replace_list(c, "EMAIL", &trimmed(emails), email_of, Typed::wants, |t| {
                typed_lines("EMAIL", t, v4, false, &mut next)
            });
        }
        if let Some(addrs) = &change.addresses {
            let want: Vec<Address> = addrs.iter().filter(|a| !a.is_empty()).cloned().collect();
            replace_list(c, "ADR", &want, address_of, Address::wants, |a| address_lines(a, v4, &mut next));
        }
        if let Some(b) = &change.birthday
            && *b != before.birthday
        {
            set_single(c, "BDAY", b.map(|b| Line::new(format!("BDAY:{}", birthday_value(b, v4)))));
        }
        if let Some(n) = &change.note
            && n.trim() != before.note.trim()
        {
            set_single(c, "NOTE", (!n.trim().is_empty()).then(|| Line::new(format!("NOTE:{}", escape(n.trim())))));
        }
        if let Some(want) = &change.categories {
            let want = name_set(want);
            let (mut a, mut b) = (want.clone(), before.categories.clone());
            a.sort();
            b.sort();
            if a != b {
                let value = want.iter().map(|x| escape(x)).collect::<Vec<_>>().join(",");
                set_all(c, "CATEGORIES", (!want.is_empty()).then(|| Line::new(format!("CATEGORIES:{value}"))));
            }
        }
        if let Some(p) = &change.photo
            && !same_photo(p.as_ref(), before.photo.as_ref())
        {
            set_all(c, "PHOTO", p.as_ref().map(|p| photo_line(p, v4)));
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
    let get = |x: &Option<String>| x.as_deref().unwrap_or("").trim().to_string();
    let (given, family, org) = (get(&change.given), get(&change.family), get(&change.org));
    let fname =
        Some(get(&change.name)).filter(|n| !n.is_empty()).unwrap_or_else(|| display_name(&given, &family, &org));
    if fname.is_empty() {
        return Err(CardError::NoName);
    }
    let skeleton = format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nPRODID:-//CalZone//CalZone//EN\r\nUID:{uid}\r\nFN:{}\r\nN:{};{};{};{};{}\r\nEND:VCARD\r\n",
        escape(&fname),
        escape(&family),
        escape(&given),
        escape(&get(&change.middle)),
        escape(&get(&change.prefix)),
        escape(&get(&change.suffix)),
    );
    let rest = CardChange {
        name: None,
        given: None,
        family: None,
        middle: None,
        prefix: None,
        suffix: None,
        ..change.clone()
    };
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
            vec![Typed::new(Kind::Mobile, "+44 7700 900001"), Typed::new(Kind::Home, "+44 20 7946 0001")]
        );
        assert_eq!(
            v.emails,
            vec![Typed { kind: Kind::Other, value: "ada@example.org".into(), label: Some("Lab".into()) }]
        );
        assert_eq!(
            (v.middle.as_str(), v.prefix.as_str(), v.suffix.as_str(), v.title.as_str()),
            ("Augusta", "Lady", "", "")
        );
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
        let escaped =
            "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nPHOTO;VALUE=URI:data:image/jpeg;base64\\,/9j/\r\nEND:VCARD\r\n";
        assert_eq!(
            read(escaped).unwrap().photo,
            Some(Photo::Inline { mime: "image/jpeg".into(), base64: "/9j/".into() })
        );
        let v = read(v4).unwrap();
        assert!(v.v4);
        assert_eq!(v.phones[0], Typed::new(Kind::Mobile, "+31-6-00000000"));
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
            phones: Some(vec![Typed::new(Kind::Mobile, "+44 7700 900001"), Typed::new(Kind::Work, "+44 20 0000 0002")]),
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
                birthday: Some(NaiveDate::from_ymd_opt(1815, 12, 11).map(Birthday::Full)),
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
                &CardChange {
                    birthday: Some(NaiveDate::from_ymd_opt(1990, 3, 15).map(Birthday::Full)),
                    ..Default::default()
                },
                now()
            )
            .unwrap()
            .contains("BDAY:19900315"),
            "v4 spelling"
        );
    }

    #[test]
    fn labels_stay_with_their_lines() {
        // The web's change has no labels: the labeled line is kept as it is.
        let v = read(V3).unwrap();
        let plain: Vec<Typed> = v.emails.iter().map(|t| Typed::new(t.kind, t.value.clone())).collect();
        assert_eq!(apply(V3, &CardChange { emails: Some(plain), ..Default::default() }, now()).unwrap(), V3);
        // Removing the labeled line takes its label along.
        let out = apply(V3, &CardChange { emails: Some(vec![]), ..Default::default() }, now()).unwrap();
        assert!(!out.contains("item1."), "{out}");
        // A new labeled line gets the next free group.
        let mut phones = v.phones.clone();
        phones.push(Typed { kind: Kind::Other, value: "+31 20 000".into(), label: Some("Boat".into()) });
        let out = apply(V3, &CardChange { phones: Some(phones), ..Default::default() }, now()).unwrap();
        assert!(out.contains("item2.TEL:+31 20 000\r\nitem2.X-ABLabel:Boat\r\n"), "{out}");
        assert_eq!(read(&out).unwrap().phones[2].label.as_deref(), Some("Boat"));
        // Another label on the same number is a different entry.
        let relabeled =
            vec![Typed { kind: Kind::Other, value: "ada@example.org".into(), label: Some("Home lab".into()) }];
        let out = apply(V3, &CardChange { emails: Some(relabeled), ..Default::default() }, now()).unwrap();
        assert!(out.contains("item2.EMAIL:ada@example.org\r\nitem2.X-ABLabel:Home lab\r\n") && !out.contains("item1."));
    }

    #[test]
    fn name_parts_title_groups_photo_and_years() {
        let out = apply(
            V3,
            &CardChange {
                middle: Some("Byron".into()),
                title: Some("Countess".into()),
                categories: Some(vec!["Family".into(), " ".into(), "Science, maths".into(), "Family".into()]),
                ..Default::default()
            },
            now(),
        )
        .unwrap();
        assert!(out.contains("N:Lovelace;Ada;Byron;Lady;\r\n") && out.contains("FN:Ada Lovelace\r\n"));
        assert!(out.contains("TITLE:Countess\r\n") && out.contains("CATEGORIES:Family,Science\\, maths\r\n"));
        let v = read(&out).unwrap();
        assert_eq!(v.categories, vec!["Family".to_string(), "Science, maths".to_string()]);
        // The same set in another order is no change.
        let again =
            CardChange { categories: Some(vec!["Science, maths".into(), "Family".into()]), ..Default::default() };
        assert_eq!(apply(&out, &again, now()).unwrap(), out);
        // A shown name made from every part follows them.
        let full = V3.replace("FN:Ada Lovelace", "FN:Lady Ada Augusta Lovelace");
        let out = apply(&full, &CardChange { family: Some("King".into()), ..Default::default() }, now()).unwrap();
        assert!(out.contains("FN:Lady Ada Augusta King\r\n"), "{out}");
        // A chosen shown name.
        let out = apply(V3, &CardChange { name: Some("Ada".into()), ..Default::default() }, now()).unwrap();
        assert!(out.contains("FN:Ada\r\n") && out.contains("N:Lovelace;Ada;Augusta;Lady;\r\n"));
        // Photos: the same data (folded differently) is no change; a new one
        // takes the old one's place; none removes it.
        let same = Photo::Inline { mime: "image/jpeg".into(), base64: "/9j/4AAQ SkZJRg==".into() };
        assert_eq!(apply(V3, &CardChange { photo: Some(Some(same)), ..Default::default() }, now()).unwrap(), V3);
        let new = Photo::Inline { mime: "image/png".into(), base64: "iVBOR".into() };
        let out = apply(V3, &CardChange { photo: Some(Some(new.clone())), ..Default::default() }, now()).unwrap();
        assert!(out.contains("PHOTO;ENCODING=b;TYPE=PNG:iVBOR\r\n") && !out.contains("SkZJRg"));
        assert_eq!(read(&out).unwrap().photo, Some(new));
        let out = apply(V3, &CardChange { photo: Some(None), ..Default::default() }, now()).unwrap();
        assert!(!out.contains("PHOTO"));
        // A birthday without a year, in each version's spelling.
        let no_year = Some(Some(Birthday::NoYear { month: 3, day: 15 }));
        let out = apply(V3, &CardChange { birthday: no_year, ..Default::default() }, now()).unwrap();
        assert!(out.contains("BDAY:--03-15\r\n"));
        assert_eq!(read(&out).unwrap().birthday, Some(Birthday::NoYear { month: 3, day: 15 }));
        let v4 = "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:X\r\nBDAY:19900315\r\nEND:VCARD\r\n";
        assert!(
            apply(v4, &CardChange { birthday: no_year, ..Default::default() }, now())
                .unwrap()
                .contains("BDAY:--0315\r\n")
        );
    }

    #[test]
    fn new_cards_are_vcard_3() {
        let change = CardChange {
            given: Some("Bea".into()),
            family: Some("Smith; Jr".into()),
            phones: Some(vec![Typed::new(Kind::Mobile, "+31 6 1"), Typed::new(Kind::Home, " ")]),
            emails: Some(vec![Typed::new(Kind::Work, "bea@example.org")]),
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
