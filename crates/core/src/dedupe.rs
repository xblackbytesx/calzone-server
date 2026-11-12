//! Contacts clean-up (decision 56): which cards in one address book are
//! probably the same person, and several cards folded into one without
//! losing anything.
//!
//! Cards match on a phone number (its last nine digits, so `+31 6 1234 5678`
//! and `06-12345678` meet), an email address (without case) or a name
//! (without case, accents, punctuation or word order, so "de Vries, Anna"
//! meets "Anna de Vries"). A number, address or name on more than
//! [`MAX_SHARED`] cards is a switchboard, a shared family line or a
//! placeholder an app filled in ("New Contact" on hundreds of saved email
//! senders), not a duplicate, and is left out. Pairs someone marked as
//! different people never join.
//!
//! A merge keeps one card (its UID, so phones update that contact) and
//! applies the chosen fields to it through [`card::apply`]. Everything
//! the form doesn't show (websites, messenger handles, anniversaries,
//! labeled extras) comes over from the other cards line by line.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use chrono::NaiveDateTime;
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

use crate::card::{self, Address, Birthday, CardChange, CardError, CardView, Photo, Typed};
use crate::lines::{Component, Document, Item, Line};

/// A phone number, email address or name on more cards than this is
/// shared (or a placeholder), not a sign of duplicates.
pub const MAX_SHARED: usize = 6;

/// The most cards one merge takes.
pub const MAX_MERGE: usize = 10;

/// The last nine digits of a phone number, when it has at least seven.
pub fn phone_key(s: &str) -> Option<String> {
    let d: String = s.chars().filter(char::is_ascii_digit).collect();
    (d.len() >= 7).then(|| d[d.len().saturating_sub(9)..].to_string())
}

/// An email address without case or spaces around it.
pub fn email_key(s: &str) -> Option<String> {
    let e = s.trim().to_lowercase();
    (e.contains('@') && e.len() >= 3).then_some(e)
}

/// Lowercase, without accents (`é` is `e`, `ß` is `ss`).
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.nfd().filter(|c| !is_combining_mark(*c)) {
        match c {
            'ß' => out.push_str("ss"),
            'æ' | 'Æ' => out.push_str("ae"),
            'œ' | 'Œ' => out.push_str("oe"),
            'ø' | 'Ø' => out.push('o'),
            'ł' | 'Ł' => out.push('l'),
            'đ' | 'Đ' => out.push('d'),
            'ı' => out.push('i'),
            c => out.extend(c.to_lowercase()),
        }
    }
    out
}

/// The shown name's words, folded and sorted; `None` for a name too short
/// to say anything.
pub fn name_key(v: &CardView) -> Option<String> {
    let source = if v.name.trim().is_empty() { format!("{} {}", v.given, v.family) } else { v.name.clone() };
    let mut words: Vec<String> =
        fold(&source).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_string).collect();
    words.sort();
    let key = words.join(" ");
    (key.chars().filter(|c| c.is_alphanumeric()).count() >= 3).then_some(key)
}

/// Why cards were grouped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Reason {
    Phone,
    Email,
    Name,
}

/// Cards that are probably one person: indices into the list given to
/// [`groups`], in order, with every reason that joined them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    pub members: Vec<usize>,
    pub reasons: Vec<Reason>,
}

fn find(parent: &mut [usize], i: usize) -> usize {
    let mut r = i;
    while parent[r] != r {
        r = parent[r];
    }
    let mut i = i;
    while parent[i] != r {
        let next = parent[i];
        parent[i] = r;
        i = next;
    }
    r
}

/// The likely duplicates among one address book's cards. `apart(i, j)`
/// (with `i < j`) says someone marked that pair as different people.
/// Groups with more reasons come first, then in card order.
pub fn groups(cards: &[CardView], apart: impl Fn(usize, usize) -> bool) -> Vec<Group> {
    let mut buckets: BTreeMap<(Reason, String), Vec<usize>> = BTreeMap::new();
    for (i, v) in cards.iter().enumerate() {
        let mut keys: BTreeSet<(Reason, String)> = BTreeSet::new();
        keys.extend(v.phones.iter().filter_map(|p| phone_key(&p.value)).map(|k| (Reason::Phone, k)));
        keys.extend(v.emails.iter().filter_map(|e| email_key(&e.value)).map(|k| (Reason::Email, k)));
        keys.extend(name_key(v).map(|k| (Reason::Name, k)));
        for k in keys {
            buckets.entry(k).or_default().push(i);
        }
    }
    let mut parent: Vec<usize> = (0..cards.len()).collect();
    let mut edges: Vec<(usize, Reason)> = Vec::new();
    for ((reason, _), members) in &buckets {
        if members.len() < 2 || members.len() > MAX_SHARED {
            continue;
        }
        for (x, &i) in members.iter().enumerate() {
            for &j in &members[x + 1..] {
                if apart(i, j) {
                    continue;
                }
                let (a, b) = (find(&mut parent, i), find(&mut parent, j));
                if a != b {
                    parent[a.max(b)] = a.min(b);
                }
                edges.push((i, *reason));
            }
        }
    }
    let mut by_root: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for i in 0..cards.len() {
        let r = find(&mut parent, i);
        by_root.entry(r).or_default().push(i);
    }
    let mut reasons: HashMap<usize, BTreeSet<Reason>> = HashMap::new();
    for (i, r) in edges {
        let root = find(&mut parent, i);
        reasons.entry(root).or_default().insert(r);
    }
    let mut out: Vec<Group> = by_root
        .into_iter()
        .filter(|(_, m)| m.len() > 1)
        .map(|(root, members)| Group {
            members,
            reasons: reasons.remove(&root).map(|s| s.into_iter().collect()).unwrap_or_default(),
        })
        .collect();
    out.sort_by_key(|g| (std::cmp::Reverse(g.reasons.len()), g.members[0]));
    out
}

/// How much a card holds: the most complete one is kept by default.
pub fn completeness(v: &CardView) -> usize {
    let filled = |s: &str| usize::from(!s.trim().is_empty());
    v.phones.len()
        + v.emails.len()
        + v.addresses.len()
        + v.categories.len()
        + filled(&v.given)
        + filled(&v.family)
        + filled(&v.org)
        + filled(&v.title)
        + filled(&v.note)
        + usize::from(v.birthday.is_some())
        + 2 * usize::from(v.photo.is_some())
}

/// The card to keep by default: the most complete, the first on a tie.
pub fn keeper(cards: &[CardView]) -> usize {
    let mut best = 0;
    for (i, v) in cards.iter().enumerate() {
        if completeness(v) > completeness(&cards[best]) {
            best = i;
        }
    }
    best
}

/// Every value the merged card can take, each with the card it comes
/// from. The kept card's values come first, so the first of each list is
/// the default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposal {
    pub keep: usize,
    /// Cards whose names differ, by card index.
    pub names: Vec<usize>,
    pub orgs: Vec<(usize, String)>,
    pub titles: Vec<(usize, String)>,
    pub birthdays: Vec<(usize, Birthday)>,
    /// Cards with a different photo, by card index.
    pub photos: Vec<usize>,
    pub phones: Vec<(usize, Typed)>,
    pub emails: Vec<(usize, Typed)>,
    pub addresses: Vec<(usize, Address)>,
    pub categories: Vec<String>,
    /// Every different note, joined by a blank line.
    pub note: String,
}

fn order(n: usize, keep: usize) -> Vec<usize> {
    std::iter::once(keep).chain((0..n).filter(|i| *i != keep)).collect()
}

fn name_parts(v: &CardView) -> String {
    fold(&[&v.name, &v.prefix, &v.given, &v.middle, &v.family, &v.suffix].map(|s| s.trim()).join("|"))
}

fn address_key(a: &Address) -> String {
    fold(&a.one_line()).split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
}

fn photo_key(p: &Photo) -> String {
    match p {
        Photo::Inline { base64, .. } => base64.split_whitespace().collect(),
        Photo::Uri(u) => u.trim().to_string(),
    }
}

pub fn proposal(cards: &[CardView], keep: usize) -> Proposal {
    let ord = order(cards.len(), keep);
    let mut p = Proposal {
        keep,
        names: Vec::new(),
        orgs: Vec::new(),
        titles: Vec::new(),
        birthdays: Vec::new(),
        photos: Vec::new(),
        phones: Vec::new(),
        emails: Vec::new(),
        addresses: Vec::new(),
        categories: Vec::new(),
        note: String::new(),
    };
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut once = |kind: &str, key: String| seen.insert(format!("{kind}\u{0}{key}"));
    let mut notes: Vec<String> = Vec::new();
    for &i in &ord {
        let v = &cards[i];
        if once("name", name_parts(v)) {
            p.names.push(i);
        }
        if !v.org.trim().is_empty() && once("org", fold(v.org.trim())) {
            p.orgs.push((i, v.org.trim().to_string()));
        }
        if !v.title.trim().is_empty() && once("title", fold(v.title.trim())) {
            p.titles.push((i, v.title.trim().to_string()));
        }
        if let Some(b) = v.birthday
            && once("bday", format!("{b:?}"))
        {
            p.birthdays.push((i, b));
        }
        if let Some(ph) = &v.photo
            && once("photo", photo_key(ph))
        {
            p.photos.push(i);
        }
        for t in &v.phones {
            let key = phone_key(&t.value).unwrap_or_else(|| t.value.trim().to_string());
            if !t.value.trim().is_empty() && once("tel", key) {
                p.phones.push((i, t.clone()));
            }
        }
        for t in &v.emails {
            let key = email_key(&t.value).unwrap_or_else(|| t.value.trim().to_string());
            if !t.value.trim().is_empty() && once("email", key) {
                p.emails.push((i, t.clone()));
            }
        }
        for a in &v.addresses {
            if !a.is_empty() && once("adr", address_key(a)) {
                p.addresses.push((i, a.clone()));
            }
        }
        for c in &v.categories {
            if once("cat", fold(c.trim())) {
                p.categories.push(c.trim().to_string());
            }
        }
        let n = v.note.trim();
        if !n.is_empty() && !notes.iter().any(|x| x.contains(n)) {
            notes.retain(|x| !n.contains(x.as_str()));
            notes.push(n.to_string());
        }
    }
    p.note = notes.join("\n\n");
    p
}

/// What the person picked on the merge page. Single fields are indices
/// into the proposal's lists (`None` leaves the field empty); `name` and
/// `photo` are card indices; lists hold the indices of the entries kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Picks {
    pub name: usize,
    pub org: Option<usize>,
    pub title: Option<usize>,
    pub birthday: Option<usize>,
    pub photo: Option<usize>,
    pub phones: Vec<usize>,
    pub emails: Vec<usize>,
    pub addresses: Vec<usize>,
    pub categories: Vec<usize>,
    pub note: String,
}

impl Picks {
    /// Everything, with the first of each choice: what the page starts with.
    pub fn all(p: &Proposal) -> Picks {
        Picks {
            name: p.names.first().copied().unwrap_or(p.keep),
            org: (!p.orgs.is_empty()).then_some(0),
            title: (!p.titles.is_empty()).then_some(0),
            birthday: (!p.birthdays.is_empty()).then_some(0),
            photo: p.photos.first().copied(),
            phones: (0..p.phones.len()).collect(),
            emails: (0..p.emails.len()).collect(),
            addresses: (0..p.addresses.len()).collect(),
            categories: (0..p.categories.len()).collect(),
            note: p.note.clone(),
        }
    }
}

/// The change that turns the kept card into the merged one. Indices out
/// of range are ignored.
pub fn change(cards: &[CardView], p: &Proposal, picks: &Picks) -> CardChange {
    let n = cards.get(picks.name).unwrap_or(&cards[p.keep]);
    let pick = |list: &[(usize, String)], i: Option<usize>| {
        i.and_then(|i| list.get(i)).map(|(_, v)| v.clone()).unwrap_or_default()
    };
    let take = |want: &[usize], len: usize| -> Vec<usize> {
        let mut v: Vec<usize> = want.iter().copied().filter(|i| *i < len).collect();
        v.sort_unstable();
        v.dedup();
        v
    };
    CardChange {
        name: Some(n.name.clone()),
        given: Some(n.given.clone()),
        family: Some(n.family.clone()),
        middle: Some(n.middle.clone()),
        prefix: Some(n.prefix.clone()),
        suffix: Some(n.suffix.clone()),
        org: Some(pick(&p.orgs, picks.org)),
        title: Some(pick(&p.titles, picks.title)),
        phones: Some(take(&picks.phones, p.phones.len()).into_iter().map(|i| p.phones[i].1.clone()).collect()),
        emails: Some(take(&picks.emails, p.emails.len()).into_iter().map(|i| p.emails[i].1.clone()).collect()),
        addresses: Some(
            take(&picks.addresses, p.addresses.len()).into_iter().map(|i| p.addresses[i].1.clone()).collect(),
        ),
        birthday: Some(picks.birthday.and_then(|i| p.birthdays.get(i)).map(|(_, b)| *b)),
        note: Some(picks.note.trim().to_string()),
        categories: Some(
            take(&picks.categories, p.categories.len()).into_iter().map(|i| p.categories[i].clone()).collect(),
        ),
        photo: Some(picks.photo.and_then(|i| cards.get(i)).and_then(|v| v.photo.clone())),
    }
}

/// Lines the merge form covers (or that belong to the kept card alone);
/// they never come over from the other cards.
const COVERED: &[&str] = &[
    "VERSION",
    "PRODID",
    "UID",
    "REV",
    "FN",
    "N",
    "ORG",
    "TITLE",
    "TEL",
    "EMAIL",
    "ADR",
    "BDAY",
    "NOTE",
    "PHOTO",
    "CATEGORIES",
    "X-ABLABEL",
    "CLIENTPIDMAP",
    "X-ABUID",
    "SORT-STRING",
];

/// Lines a card holds at most once: one from another card comes over only
/// when the kept card has none.
const SINGLE: &[&str] = &["KIND", "GENDER", "ANNIVERSARY", "X-ANNIVERSARY", "X-ABSHOWAS", "TZ", "GEO", "LANG"];

/// A line's value for comparing: unescaped, folded, trimmed.
fn same_value(a: &Line, b: &Line) -> bool {
    a.name() == b.name() && fold(card::unescape(a.value()).trim()) == fold(card::unescape(b.value()).trim())
}

fn has_line(c: &Component, line: &Line) -> bool {
    c.properties().any(|l| same_value(l, line))
}

/// The text of a grouped line under another group name.
fn regroup(line: &Line, group: &str) -> Line {
    let text = line.text();
    let rest = text.split_once('.').map_or(text, |(_, r)| r);
    Line::new(format!("{group}.{rest}"))
}

/// Merge `others` into `keep`: the chosen fields through the lossless
/// editor, then every line the form doesn't cover that the kept card
/// lacks, labeled groups (`item3.URL` with its `X-ABLabel`) kept together
/// under a new group name.
pub fn merge(keep: &str, others: &[&str], change: &CardChange, now: NaiveDateTime) -> Result<String, CardError> {
    let edited = card::apply(keep, change, now)?;
    let mut doc = Document::parse(&edited).map_err(|_| CardError::NotACard)?;
    let mut added = false;
    {
        let c = doc.components.first_mut().filter(|c| c.name == "VCARD").ok_or(CardError::NotACard)?;
        let mut next = card::next_group(c);
        for other in others {
            let odoc = Document::parse(other).map_err(|_| CardError::NotACard)?;
            let oc = odoc.components.first().filter(|c| c.name == "VCARD").ok_or(CardError::NotACard)?;
            // Groups in the order they first appear.
            let mut grouped: Vec<(String, Vec<&Line>)> = Vec::new();
            for line in oc.properties() {
                match line.group() {
                    Some(g) => {
                        let g = g.to_ascii_lowercase();
                        match grouped.iter_mut().find(|(x, _)| *x == g) {
                            Some((_, lines)) => lines.push(line),
                            None => grouped.push((g, vec![line])),
                        }
                    }
                    None => {
                        let name = line.name();
                        if COVERED.contains(&name.as_str())
                            || (SINGLE.contains(&name.as_str()) && c.property(&name).is_some())
                            || has_line(c, line)
                        {
                            continue;
                        }
                        let at = card::insert_at(c, &name);
                        c.items.insert(at, Item::Property(Line::new(line.text())));
                        added = true;
                    }
                }
            }
            for (_, lines) in grouped {
                let main: Vec<&&Line> = lines.iter().filter(|l| l.name() != "X-ABLABEL").collect();
                // A labeled phone, email or address came through the form.
                if main.is_empty()
                    || main.iter().any(|l| COVERED.contains(&l.name().as_str()))
                    || main.iter().all(|l| has_line(c, l))
                {
                    continue;
                }
                let g = format!("item{next}");
                next += 1;
                let at = c.items.len();
                for (k, l) in lines.iter().enumerate() {
                    c.items.insert(at + k, Item::Property(regroup(l, &g)));
                }
                added = true;
            }
        }
    }
    if !added {
        return Ok(edited);
    }
    let c = doc.components.first_mut().expect("checked above");
    card::set_single(c, "REV", Some(card::rev(now)));
    Ok(doc.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::card::Kind;
    use chrono::NaiveDate;

    fn now() -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 11, 12).unwrap().and_hms_opt(9, 0, 0).unwrap()
    }

    fn view(name: &str, phones: &[&str], emails: &[&str]) -> CardView {
        CardView {
            name: name.into(),
            phones: phones.iter().map(|p| Typed::new(Kind::Mobile, *p)).collect(),
            emails: emails.iter().map(|e| Typed::new(Kind::Home, *e)).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn keys() {
        assert_eq!(phone_key("+31 6 1234 5678"), phone_key("06-12345678"));
        assert_eq!(phone_key("0031 (0)6 12345678"), phone_key("06 12 34 56 78"));
        assert_eq!(phone_key("112"), None);
        assert_ne!(phone_key("+31 20 123 4567"), phone_key("+31 6 1234 5678"));
        assert_eq!(email_key(" Anna@Example.ORG "), Some("anna@example.org".into()));
        assert_eq!(email_key("nope"), None);
        assert_eq!(fold("Émile Ørsted Straße"), "emile orsted strasse");
        let a = view("de Vries, Anna", &[], &[]);
        let b = view("Anna  de VRIES", &[], &[]);
        let c = view("Anna de Vriës", &[], &[]);
        assert_eq!(name_key(&a), name_key(&b));
        assert_eq!(name_key(&a), name_key(&c));
        assert_eq!(name_key(&view("Jo", &[], &[])), None);
        let from_parts = CardView { given: "Anna".into(), family: "de Vries".into(), ..Default::default() };
        assert_eq!(name_key(&from_parts), name_key(&a));
    }

    #[test]
    fn grouping() {
        let cards = vec![
            view("Anna de Vries", &["+31 6 1234 5678"], &[]),
            view("Bram", &[], &["bram@example.org"]),
            view("A. de Vries", &["06-12345678"], &["anna@example.org"]),
            view("Anna", &[], &["ANNA@example.org"]),
            view("Bram de Vries", &[], &["Bram@Example.org"]),
            view("Someone else", &["+31 20 000 0000"], &[]),
            view("de Vries, Anna", &[], &[]),
        ];
        let g = groups(&cards, |_, _| false);
        // Anna's four cards through phone, email and name; Bram's two by email.
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].members, vec![0, 2, 3, 6]);
        assert_eq!(g[0].reasons, vec![Reason::Phone, Reason::Email, Reason::Name]);
        assert_eq!(g[1], Group { members: vec![1, 4], reasons: vec![Reason::Email] });
        // A pair marked apart stays apart unless something else joins it.
        let g = groups(&cards, |i, j| (i, j) == (1, 4));
        assert_eq!(g.len(), 1);
        let g = groups(&cards, |i, j| (i, j) == (0, 6));
        assert_eq!(g[0].members, vec![0, 2, 3], "card 6 only matched card 0 by name");
    }

    #[test]
    fn shared_numbers_are_not_duplicates() {
        let mut cards: Vec<CardView> =
            (0..=MAX_SHARED).map(|i| view(&format!("Colleague {i}"), &["+31 20 555 0000"], &[])).collect();
        assert!(groups(&cards, |_, _| false).is_empty(), "a switchboard");
        let placeholders: Vec<CardView> =
            (0..=MAX_SHARED).map(|i| view("New Contact", &[], &[&format!("{i}@example.org")])).collect();
        assert!(groups(&placeholders, |_, _| false).is_empty(), "a name an app filled in");
        cards.truncate(2);
        assert_eq!(groups(&cards, |_, _| false).len(), 1);
    }

    const KEEP: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:keep\r\nFN:Anna de Vries\r\nN:de Vries;Anna;;;\r\n\
TEL;TYPE=CELL:+31 6 1234 5678\r\nEMAIL;TYPE=HOME:anna@example.org\r\nitem1.URL:https://anna.example\r\nitem1.X-ABLabel:Blog\r\n\
X-KEEP:mine\r\nEND:VCARD\r\n";

    const OTHER: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:other\r\nFN:A. de Vries\r\nN:de Vries;A.;;;\r\n\
TEL;TYPE=cell:06-12345678\r\nTEL;TYPE=work:+31 20 123 4567\r\nitem1.EMAIL:work@example.org\r\nitem1.X-ABLabel:Office\r\n\
item2.URL:https://anna.example\r\nitem2.X-ABLabel:Blog\r\nitem3.X-ABRELATEDNAMES:Bram\r\nitem3.X-ABLabel:_$!<Spouse>!$_\r\n\
IMPP:xmpp:anna@example.org\r\nANNIVERSARY:20100605\r\nX-KEEP:mine\r\nBDAY:19850304\r\nNOTE:Met at school\r\n\
CATEGORIES:Family\r\nEND:VCARD\r\n";

    #[test]
    fn proposals_and_merges() {
        let cards = vec![card::read(KEEP).unwrap(), card::read(OTHER).unwrap()];
        assert_eq!(keeper(&cards), 1, "the fuller card");
        let p = proposal(&cards, 0);
        assert_eq!(p.names, vec![0, 1]);
        assert_eq!(p.phones.len(), 2, "the same mobile once");
        assert_eq!(p.phones[1].1.value, "+31 20 123 4567");
        assert_eq!(
            p.emails.iter().map(|(_, e)| e.label.clone()).collect::<Vec<_>>(),
            vec![None, Some("Office".into())]
        );
        assert_eq!(p.note, "Met at school");
        let picks = Picks::all(&p);
        let out = merge(KEEP, &[OTHER], &change(&cards, &p, &picks), now()).unwrap();
        let v = card::read(&out).unwrap();
        assert_eq!(v.uid.as_deref(), Some("keep"));
        assert_eq!(v.name, "Anna de Vries");
        assert_eq!(v.phones.len(), 2);
        assert_eq!(v.emails.len(), 2);
        assert_eq!(v.emails[1].label.as_deref(), Some("Office"));
        assert!(v.birthday.is_some() && v.note == "Met at school" && v.categories == vec!["Family".to_string()]);
        // Lines the form doesn't show came over; repeats didn't.
        for want in
            ["IMPP:xmpp:anna@example.org", "ANNIVERSARY:20100605", "X-ABRELATEDNAMES:Bram", "REV:20261112T090000Z"]
        {
            assert!(out.contains(want), "{want} in {out}");
        }
        assert_eq!(out.matches("X-KEEP:mine").count(), 1);
        assert_eq!(out.matches("URL:https://anna.example").count(), 1);
        assert!(out.contains("X-ABLabel:_$!<Spouse>!$_"));
        // The relation got a group of its own next to the kept card's item1.
        let rel = out.lines().find(|l| l.contains("X-ABRELATEDNAMES")).unwrap();
        assert!(!rel.starts_with("item1."), "{rel}");
        let g = rel.split('.').next().unwrap();
        assert!(out.contains(&format!("{g}.X-ABLabel:_$!<Spouse>!$_")));
        crate::object::inspect_card(&Document::parse(&out).unwrap()).unwrap();
        // The other name, chosen; a phone and the note left out.
        let picks = Picks { name: 1, phones: vec![1], note: String::new(), ..Picks::all(&p) };
        let v = card::read(&merge(KEEP, &[OTHER], &change(&cards, &p, &picks), now()).unwrap()).unwrap();
        assert_eq!((v.name.as_str(), v.given.as_str()), ("A. de Vries", "A."));
        assert_eq!(v.phones.iter().map(|t| t.value.as_str()).collect::<Vec<_>>(), vec!["+31 20 123 4567"]);
        assert!(v.note.is_empty());
    }

    #[test]
    fn notes_join_without_repeats() {
        let mut a = view("Anna", &[], &[]);
        a.note = "Allergic to nuts".into();
        let mut b = view("Anna", &[], &[]);
        b.note = "Allergic to nuts. Birthday in May".into();
        let mut c = view("Anna", &[], &[]);
        c.note = "Neighbor".into();
        let p = proposal(&[a, b, c], 0);
        assert_eq!(p.note, "Allergic to nuts. Birthday in May\n\nNeighbor");
    }
}
