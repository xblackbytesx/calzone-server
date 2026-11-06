//! Files in and out (decision 30): `.ics` and `.vcf` split into stored
//! objects, and stored objects assembled back into one file per collection.
//!
//! - [`decode`] turns uploaded bytes into text, first repairing folds that
//!   split a UTF-8 character (some writers fold at 75 bytes without looking
//!   at characters, which leaves bytes that are not UTF-8 until unfolded).
//! - [`split_calendar`] makes one CalDAV object per UID (a master and its
//!   overrides together) with the VTIMEZONEs that object uses, and drops
//!   `METHOD`, which a stored object must not have (RFC 4791 4.1).
//! - [`split_cards`] makes one object per VCARD, bytes unchanged.
//! - [`assemble_calendar`] and [`assemble_cards`] write a collection back
//!   out as one file.

use std::borrow::Cow;
use std::collections::{BTreeSet, HashMap};

use crate::lines::{Component, Document, Item, Line, ParseError};

/// Fold markers: a line ending plus one space or tab. The `&#13;` forms are
/// the same fold inside XML text (a CalDAV REPORT answer), where a server
/// may write CR as a character reference.
const FOLDS: [&[u8]; 8] = [b"\r\n ", b"\r\n\t", b"\n ", b"\n\t", b"&#13;\n ", b"&#13;\n\t", b"&#xD;\n ", b"&#xd;\n "];

/// Remove folds that split a UTF-8 character, so the bytes decode. Only a
/// fold with an incomplete character before it and a continuation byte
/// after it is removed; every other byte stays where it was. Returns the
/// bytes and the number of folds removed (borrowed when nothing changed).
pub fn repair_split_folds(input: &[u8]) -> (Cow<'_, [u8]>, usize) {
    // `out` exists once the first fold is removed and is current from then on.
    let mut out: Option<Vec<u8>> = None;
    let mut repaired = 0;
    let mut i = 0;
    while i < input.len() {
        if matches!(input[i], b'\r' | b'\n' | b'&')
            && let Some(n) = FOLDS.iter().find(|f| input[i..].starts_with(f)).map(|f| f.len())
            && incomplete_tail(out.as_deref().unwrap_or(&input[..i]))
            && input.get(i + n).copied().is_some_and(is_continuation)
        {
            out.get_or_insert_with(|| input[..i].to_vec());
            repaired += 1;
            i += n;
            continue;
        }
        if let Some(o) = &mut out {
            o.push(input[i]);
        }
        i += 1;
    }
    match out {
        Some(o) => (Cow::Owned(o), repaired),
        None => (Cow::Borrowed(input), 0),
    }
}

fn is_continuation(b: u8) -> bool {
    (0x80..=0xBF).contains(&b)
}

/// Whether `bytes` ends inside a multi-byte UTF-8 character.
fn incomplete_tail(bytes: &[u8]) -> bool {
    for back in 1..=3.min(bytes.len()) {
        let b = bytes[bytes.len() - back];
        if is_continuation(b) {
            continue;
        }
        let needed = match b {
            0xC2..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF4 => 4,
            _ => return false,
        };
        return back < needed;
    }
    false
}

/// Bytes that are still not UTF-8 after repairing split folds.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not UTF-8 text (byte {offset})")]
pub struct NotUtf8 {
    pub offset: usize,
}

/// Text from uploaded bytes, and how many split folds were repaired.
pub fn decode(bytes: &[u8]) -> Result<(String, usize), NotUtf8> {
    let (fixed, repaired) = repair_split_folds(bytes);
    match std::str::from_utf8(&fixed) {
        Ok(s) => Ok((s.to_string(), repaired)),
        Err(e) => Err(NotUtf8 { offset: e.valid_up_to() }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FileError {
    #[error(transparent)]
    Parse(#[from] ParseError),
    #[error("this is not a {0} file")]
    WrongKind(&'static str),
}

/// A calendar file split into objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarFile {
    /// `X-WR-CALNAME` or `NAME` (RFC 7986), unescaped.
    pub name: Option<String>,
    /// One VCALENDAR per UID. A component without UID is an object of its
    /// own, so the store can refuse it and the importer can keep it.
    pub objects: Vec<String>,
}

/// One object per UID, each with its calendar's properties (`METHOD`
/// dropped) and the VTIMEZONEs its lines refer to. Several VCALENDARs in one
/// file are read in turn; a UID seen in an earlier one gathers later
/// components too.
pub fn split_calendar(text: &str) -> Result<CalendarFile, FileError> {
    let doc = Document::parse(text)?;
    if doc.components.iter().any(|c| c.name != "VCALENDAR") {
        return Err(FileError::WrongKind("calendar"));
    }
    let mut name = None;
    let mut zones: HashMap<String, Component> = HashMap::new();
    // (calendar index, components) per group, in order of first appearance.
    let mut groups: Vec<(usize, Vec<Item>)> = Vec::new();
    let mut by_uid: HashMap<String, usize> = HashMap::new();
    for (ci, cal) in doc.components.iter().enumerate() {
        for item in &cal.items {
            match item {
                Item::Property(l) if name.is_none() && matches!(l.name().as_str(), "X-WR-CALNAME" | "NAME") => {
                    name = Some(crate::object::unescape(l.value())).filter(|n| !n.trim().is_empty());
                }
                Item::Property(_) => {}
                Item::Component(c) if c.name == "VTIMEZONE" => {
                    if let Some(id) = c.property("TZID").map(|l| l.value().to_string()) {
                        zones.entry(id).or_insert_with(|| c.clone());
                    }
                }
                Item::Component(c) => {
                    let uid = c.property("UID").map(|l| l.value().to_string()).filter(|u| !u.is_empty());
                    match uid.as_ref().and_then(|u| by_uid.get(u)) {
                        Some(&g) => groups[g].1.push(item.clone()),
                        None => {
                            if let Some(u) = uid {
                                by_uid.insert(u, groups.len());
                            }
                            groups.push((ci, vec![item.clone()]));
                        }
                    }
                }
            }
        }
    }
    let objects = groups
        .into_iter()
        .map(|(ci, parts)| {
            let cal = &doc.components[ci];
            let mut used = BTreeSet::new();
            for p in &parts {
                if let Item::Component(c) = p {
                    tzids(c, &mut used);
                }
            }
            let mut items: Vec<Item> =
                cal.items.iter().filter(|i| matches!(i, Item::Property(l) if l.name() != "METHOD")).cloned().collect();
            items.extend(used.iter().filter_map(|id| zones.get(id)).map(|z| Item::Component(z.clone())));
            items.extend(parts);
            Component { items, ..cal.clone() }.to_string()
        })
        .collect();
    Ok(CalendarFile { name, objects })
}

/// Every `TZID=` parameter value in a component tree.
fn tzids(c: &Component, out: &mut BTreeSet<String>) {
    for item in &c.items {
        match item {
            Item::Property(l) => {
                if let Some(id) = l.param("TZID") {
                    out.insert(id);
                }
            }
            Item::Component(inner) => tzids(inner, out),
        }
    }
}

/// One object per VCARD, each exactly as it was in the file.
pub fn split_cards(text: &str) -> Result<Vec<String>, FileError> {
    let doc = Document::parse(text)?;
    if doc.components.iter().any(|c| c.name != "VCARD") {
        return Err(FileError::WrongKind("contacts"));
    }
    Ok(doc.components.iter().map(Component::to_string).collect())
}

/// RFC 5545 TEXT escaping.
fn escape_text(s: &str) -> String {
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

/// One VCALENDAR holding every object's components, each VTIMEZONE once (by
/// TZID, first seen). The objects' own calendar properties are not repeated;
/// the file gets `VERSION`, `PRODID` and the collection name. Returns the
/// text and how many objects did not parse (stored objects always do).
pub fn assemble_calendar(name: &str, objects: &[String]) -> (String, usize) {
    let mut cal = Component::new("VCALENDAR");
    let mut props = vec![Line::new("VERSION:2.0"), Line::new("PRODID:-//CalZone//CalZone//EN")];
    if !name.is_empty() {
        props.push(Line::new(format!("X-WR-CALNAME:{}", escape_text(name))));
    }
    cal.items.extend(props.into_iter().map(Item::Property));
    let mut seen_zones = BTreeSet::new();
    let mut zones = Vec::new();
    let mut parts = Vec::new();
    let mut broken = 0;
    for o in objects {
        let Ok(doc) = Document::parse(o) else {
            broken += 1;
            continue;
        };
        for c in doc.components {
            for item in c.items {
                match item {
                    Item::Component(z) if z.name == "VTIMEZONE" => {
                        let id = z.property("TZID").map(|l| l.value().to_string()).unwrap_or_default();
                        if seen_zones.insert(id) {
                            zones.push(Item::Component(z));
                        }
                    }
                    Item::Component(part) => parts.push(Item::Component(ensure_line_end(part))),
                    Item::Property(_) => {}
                }
            }
        }
    }
    cal.items.extend(zones);
    cal.items.extend(parts);
    (cal.to_string(), broken)
}

/// A component's END line from a file that ended without a newline would
/// run into the next line when components are joined; give it one.
fn ensure_line_end(mut c: Component) -> Component {
    if c.end.is_original() && !c.end.source().ends_with('\n') {
        c.end = Line::new(c.end.text().to_string());
    }
    c
}

/// The cards one after another, each ending in a line break.
pub fn assemble_cards(cards: &[String]) -> String {
    let mut out = String::new();
    for c in cards {
        out.push_str(c);
        if !c.ends_with('\n') {
            out.push_str("\r\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unfold(s: &str) -> String {
        s.replace("\r\n ", "").replace("\r\n\t", "").replace("\n ", "").replace("\r\n", "\n")
    }

    #[test]
    fn a_fold_inside_a_character_is_repaired() {
        // "é" is C3 A9; a byte-counting writer folded between the two.
        let bytes = b"SUMMARY:Caf\xC3\r\n \xA9 ok\r\n";
        let (fixed, n) = repair_split_folds(bytes);
        assert_eq!(n, 1);
        assert_eq!(&*fixed, "SUMMARY:Café ok\r\n".as_bytes());
        assert_eq!(decode(bytes).unwrap(), ("SUMMARY:Café ok\r\n".to_string(), 1));
    }

    #[test]
    fn a_four_byte_character_split_twice_and_lf_folds() {
        // U+1F600 is F0 9F 98 80.
        let bytes = b"X:\xF0\n \x9F\x98\n\t\x80!\n";
        assert_eq!(decode(bytes).unwrap(), ("X:\u{1F600}!\n".to_string(), 2));
    }

    #[test]
    fn xml_character_reference_folds_are_repaired() {
        let bytes = b"<c>SUMMARY:\xC3&#13;\n \xA9</c>";
        assert_eq!(decode(bytes).unwrap().0, "<c>SUMMARY:é</c>");
    }

    #[test]
    fn valid_text_is_borrowed_and_ordinary_folds_stay() {
        let s = "SUMMARY:Café\r\n  long\r\n".as_bytes();
        let (fixed, n) = repair_split_folds(s);
        assert!(matches!(fixed, Cow::Borrowed(_)));
        assert_eq!(n, 0);
        // A fold right after a complete character is a real fold.
        assert_eq!(decode("A:é\r\n x\r\n".as_bytes()).unwrap().1, 0);
    }

    #[test]
    fn other_bad_bytes_are_reported_not_guessed() {
        assert_eq!(decode(b"SUMMARY:caf\xE9\r\n"), Err(NotUtf8 { offset: 11 }));
        // A fold after a stray lead byte, followed by ASCII, is left alone.
        assert!(decode(b"A:\xC3\r\n x").is_err());
    }

    const ZONE_A: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Amsterdam\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";
    const ZONE_B: &str = "BEGIN:VTIMEZONE\r\nTZID:Asia/Tokyo\r\nBEGIN:STANDARD\r\nDTSTART:19700101T000000\r\nTZOFFSETFROM:+0900\r\nTZOFFSETTO:+0900\r\nEND:STANDARD\r\nEND:VTIMEZONE\r\n";

    fn takeout() -> String {
        format!(
            "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\nCALSCALE:GREGORIAN\r\n\
             METHOD:PUBLISH\r\nX-WR-CALNAME:Family\\, home\r\nX-WR-TIMEZONE:Europe/Amsterdam\r\n{ZONE_A}{ZONE_B}\
             BEGIN:VEVENT\r\nDTSTART;TZID=Europe/Amsterdam:20260105T090000\r\nRRULE:FREQ=WEEKLY\r\nUID:weekly@google.com\r\nSUMMARY:Swim\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nDTSTART:20260301T100000Z\r\nUID:once@google.com\r\nSUMMARY:Once\r\nEND:VEVENT\r\n\
             BEGIN:VEVENT\r\nDTSTART;TZID=Europe/Amsterdam:20260113T100000\r\nRECURRENCE-ID;TZID=Europe/Amsterdam:20260112T090000\r\nUID:weekly@google.com\r\nSEQUENCE:2\r\nSUMMARY:Swim (moved)\r\nEND:VEVENT\r\n\
             BEGIN:VTODO\r\nSUMMARY:no uid\r\nEND:VTODO\r\n\
             END:VCALENDAR\r\n"
        )
    }

    #[test]
    fn a_takeout_calendar_splits_by_uid_with_its_zones() {
        let f = split_calendar(&takeout()).unwrap();
        assert_eq!(f.name.as_deref(), Some("Family, home"));
        assert_eq!(f.objects.len(), 3);
        let weekly = &f.objects[0];
        assert!(!weekly.contains("METHOD"));
        assert!(weekly.contains("X-WR-CALNAME") && weekly.contains("CALSCALE:GREGORIAN"));
        assert!(weekly.contains("TZID:Europe/Amsterdam") && !weekly.contains("Asia/Tokyo"));
        assert_eq!(weekly.matches("BEGIN:VEVENT").count(), 2);
        assert!(weekly.contains("Swim (moved)"));
        // The UTC one needs no zone; the UID-less one stays, alone.
        assert!(!f.objects[1].contains("VTIMEZONE"));
        assert!(f.objects[2].contains("no uid"));
        // Each object is a valid stored object except the UID-less one.
        for o in &f.objects[..2] {
            let doc = Document::parse(o).unwrap();
            crate::object::inspect_calendar(&doc, o).unwrap();
        }
    }

    #[test]
    fn several_calendars_in_one_file_and_wrong_kinds() {
        let two = format!("{}{}", takeout(), takeout().replace("weekly@", "other@").replace("once@", "x@"));
        let f = split_calendar(&two).unwrap();
        // weekly, once, no-uid, then other, x and the second no-uid.
        assert_eq!(f.objects.len(), 6);
        assert_eq!(split_calendar("BEGIN:VCARD\r\nEND:VCARD\r\n"), Err(FileError::WrongKind("calendar")));
        assert_eq!(split_cards(&takeout()), Err(FileError::WrongKind("contacts")));
        assert!(matches!(split_calendar("nonsense"), Err(FileError::Parse(_))));
    }

    #[test]
    fn cards_split_byte_for_byte() {
        let a = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:a\r\nFN:A\r\nEND:VCARD\r\n";
        let b = "BEGIN:VCARD\nVERSION:3.0\nFN:B\n  folded\nEND:VCARD\n";
        let cards = split_cards(&format!("{a}{b}")).unwrap();
        assert_eq!(cards, vec![a.to_string(), b.to_string()]);
        assert_eq!(assemble_cards(&cards), format!("{a}{b}"));
        assert_eq!(assemble_cards(&["BEGIN:VCARD\r\nEND:VCARD".into()]), "BEGIN:VCARD\r\nEND:VCARD\r\n");
    }

    #[test]
    fn split_then_assemble_keeps_every_component() {
        let f = split_calendar(&takeout()).unwrap();
        let (out, broken) = assemble_calendar("Family, home", &f.objects);
        assert_eq!(broken, 0);
        assert!(out.contains("X-WR-CALNAME:Family\\, home"));
        assert_eq!(out.matches("BEGIN:VTIMEZONE").count(), 1);
        let again = split_calendar(&out).unwrap();
        // Same components per object, apart from the calendar properties.
        let comps = |objs: &[String]| -> Vec<String> {
            objs.iter()
                .map(|o| {
                    let d = Document::parse(o).unwrap();
                    d.components[0]
                        .components()
                        .filter(|c| c.name != "VTIMEZONE")
                        .map(|c| unfold(&c.to_string()))
                        .collect::<String>()
                })
                .collect()
        };
        assert_eq!(comps(&f.objects), comps(&again.objects));
    }

    #[test]
    fn a_file_without_a_final_newline_still_joins_cleanly() {
        let a = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART:20260101T000000Z\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let b = a.replace("UID:a", "UID:b");
        let f = split_calendar(a).unwrap();
        let g = split_calendar(&b).unwrap();
        let (out, _) = assemble_calendar("", &[f.objects[0].clone(), g.objects[0].clone()]);
        assert_eq!(split_calendar(&out).unwrap().objects.len(), 2);
    }
}
