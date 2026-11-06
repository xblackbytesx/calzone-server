//! Every object through the whole core: lossless round trip, resource rules
//! and projection, the per-user split, and recurrence cross-checked against
//! the independent `rrule` crate.
//!
//! Synthetic fixtures live in `tests/fixtures/`. The household corpus in
//! `fixtures/private/` (repo root) is confidential: those tests print counts
//! only, and no assertion message may contain anything from the data
//! (AGENTS.md invariants). They print SKIP when the corpus is absent.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calzone_core::lines::{Component, Document, Item};
use calzone_core::object::{self, Bounds};
use calzone_core::recur::{self, When};
use calzone_core::split;
use chrono::NaiveDateTime;

fn dt(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
}

#[derive(Default, Debug)]
struct Tally {
    objects: usize,
    recurring: usize,
    cross_checked: usize,
    cross_skipped: usize,
    open_ended: usize,
    layer_entries: usize,
    occurrences: usize,
}

/// Everything the core promises about one calendar object. Panics carry an
/// index and a check name only, never content.
fn check_calendar_object(i: usize, text: &str, t: &mut Tally) {
    let doc = Document::parse(text).unwrap_or_else(|_| panic!("object {i}: does not parse"));
    assert!(doc.to_string() == text, "object {i}: round trip differs");

    let info = object::inspect_calendar(&doc, text).unwrap_or_else(|e| panic!("object {i}: refused: {e}"));
    assert!(info.warnings.is_empty(), "object {i}: warnings {:?}", info.warnings.len());
    t.objects += 1;
    if info.recurring {
        t.recurring += 1;
    }
    if matches!(info.bounds, Bounds::Span { end: None, .. }) {
        t.open_ended += 1;
    }

    let mut shared = doc.clone();
    let layer = split::split(&mut shared);
    t.layer_entries += layer.entries.len();
    let has_alarm = |c: &Component| c.components().any(|a| a.name == "VALARM");
    assert!(!shared.components[0].components().any(has_alarm), "object {i}: an alarm stayed in the shared copy");
    split::merge(&mut shared, &layer);
    assert!(shared.to_string() == text, "object {i}: split + merge differs");

    let exp = recur::expand(text, dt("1990-01-01 00:00"), dt("2035-01-01 00:00"), chrono_tz::Europe::Amsterdam)
        .unwrap_or_else(|_| panic!("object {i}: expand failed"));
    assert!(exp.warnings.is_empty(), "object {i}: expansion warnings");
    t.occurrences += exp.occurrences.len();
    if let Bounds::Span { start, end } = info.bounds {
        for o in &exp.occurrences {
            let s = o.start.instant(chrono_tz::UTC);
            assert!(s + object::FLOATING_MARGIN >= start, "object {i}: occurrence before the projection start");
            if let Some(end) = end {
                assert!(s <= end + object::FLOATING_MARGIN, "object {i}: occurrence after the projection end");
            }
        }
    }
    cross_check(i, &doc, &exp, t);
}

/// Wall times of the master's own instances, compared with the rrule crate.
/// Objects with overrides, RDATE or a non-IANA zone are skipped (rrule
/// cannot express them).
fn cross_check(i: usize, doc: &Document, exp: &recur::Expansion, t: &mut Tally) {
    let cal = &doc.components[0];
    let parts: Vec<&Component> = cal.components().filter(|c| matches!(c.name.as_str(), "VEVENT" | "VTODO")).collect();
    let [master] = parts.as_slice() else {
        t.cross_skipped += usize::from(parts.iter().any(|c| c.property("RRULE").is_some()));
        return;
    };
    let Some(rrule) = master.property("RRULE") else { return };
    if master.property("RDATE").is_some() {
        t.cross_skipped += 1;
        return;
    }
    let date_line = |l: &calzone_core::lines::Line| {
        let v = l.value();
        if l.param("VALUE").is_some_and(|v| v.eq_ignore_ascii_case("DATE")) && v.len() == 8 {
            format!("{}:{v}T000000", l.name())
        } else {
            l.text().to_string()
        }
    };
    let Some(start) = master.property("DTSTART").or_else(|| master.property("DUE")) else { return };
    let mut spec = format!("{}\n{}", date_line(start).replacen("DUE", "DTSTART", 1), rrule.text());
    for ex in master.properties().filter(|l| l.name() == "EXDATE") {
        spec.push('\n');
        spec.push_str(&date_line(ex));
    }
    let Ok(set) = spec.parse::<rrule::RRuleSet>() else {
        t.cross_skipped += 1;
        return;
    };
    let cutoff = dt("2035-01-01 00:00");
    let theirs: Vec<NaiveDateTime> = set
        .into_iter()
        .map(|d| d.naive_local())
        .filter(|d| *d >= dt("1990-01-01 00:00"))
        .take_while(|d| *d < cutoff)
        .collect();
    let ours: Vec<NaiveDateTime> = exp
        .occurrences
        .iter()
        .map(|o| match o.start {
            When::Utc(t) | When::Floating(t) | When::Zoned { local: t, .. } => t,
            When::Date(d) => d.and_hms_opt(0, 0, 0).unwrap(),
        })
        .filter(|d| *d < cutoff)
        .collect();
    assert!(ours == theirs, "object {i}: recurrence disagrees with rrule ({} vs {})", ours.len(), theirs.len());
    t.cross_checked += 1;
}

fn check_card(i: usize, text: &str) -> object::CardInfo {
    let doc = Document::parse(text).unwrap_or_else(|_| panic!("card {i}: does not parse"));
    assert!(doc.to_string() == text, "card {i}: round trip differs");
    object::inspect_card(&doc).unwrap_or_else(|e| panic!("card {i}: refused: {e}"))
}

fn fixture(name: &str) -> String {
    std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)).unwrap()
}

#[test]
fn thunderbird_series() {
    let text = fixture("thunderbird-series.ics");
    let mut t = Tally::default();
    check_calendar_object(0, &text, &mut t);
    assert_eq!(t.layer_entries, 4, "LASTACK, a snooze time and two alarms");
    let exp = recur::expand(&text, dt("2026-01-01 00:00"), dt("2027-06-01 00:00"), chrono_tz::UTC).unwrap();
    assert_eq!(exp.occurrences.len(), 9, "10 Tuesdays, one excluded");
    let moved: Vec<_> = exp.occurrences.iter().filter(|o| o.overridden).collect();
    assert_eq!(moved.len(), 1);
    assert_eq!(moved[0].start, When::Zoned { local: dt("2026-11-25 14:00"), utc: dt("2026-11-25 13:00") });
}

#[test]
fn todos() {
    let mut t = Tally::default();
    check_calendar_object(0, &fixture("todo-recurring.ics"), &mut t);
    check_calendar_object(1, &fixture("todo-undated.ics"), &mut t);
    let text = fixture("todo-undated.ics");
    let info = object::inspect_calendar(&Document::parse(&text).unwrap(), &text).unwrap();
    assert_eq!(info.bounds, Bounds::Undated);
    assert_eq!(t.cross_checked, 1);
}

#[test]
fn custom_timezone_from_1601_onsets() {
    let text = fixture("custom-timezone.ics");
    let mut t = Tally::default();
    check_calendar_object(0, &text, &mut t);
    let exp = recur::expand(&text, dt("2026-01-01 00:00"), dt("2027-01-01 00:00"), chrono_tz::UTC).unwrap();
    let utc: Vec<NaiveDateTime> = exp.occurrences.iter().map(|o| o.start.instant(chrono_tz::UTC)).collect();
    // DST starts 2026-03-29: 09:00 local is 08:00Z before, 07:00Z after.
    assert_eq!(
        utc,
        vec![
            dt("2026-03-26 08:00"),
            dt("2026-03-27 08:00"),
            dt("2026-03-28 08:00"),
            dt("2026-03-29 07:00"),
            dt("2026-03-30 07:00")
        ]
    );
}

#[test]
fn cards() {
    let c3 = check_card(0, &fixture("vcard3-photo.vcf"));
    assert_eq!(c3.version.as_deref(), Some("3.0"));
    assert_eq!(c3.formatted_name.as_deref(), Some("Zoë Synthetic"));
    let c4 = check_card(1, &fixture("vcard4.vcf"));
    assert_eq!(c4.emails, vec!["person@example.org"]);
    let doc = Document::parse(&fixture("vcard3-photo.vcf")).unwrap();
    let photo = doc.components[0].property("PHOTO").unwrap();
    assert!(photo.value().ends_with("ErkJggg=="), "the folded photo unfolds to one value");
}

// ---- the household corpus (confidential) ----

fn private_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private")
}

fn private_files(ext: &str) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(private_dir()) else { return Vec::new() };
    let mut v: Vec<PathBuf> =
        rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == ext)).collect();
    v.sort();
    v
}

/// A merged export (one VCALENDAR, many UIDs) as one object per UID, the way
/// a CalDAV server stores them: the export's own header lines and every
/// VTIMEZONE, then that UID's components.
fn objects_from_export(text: &str) -> Vec<String> {
    let doc = Document::parse(text).unwrap_or_else(|_| panic!("private export does not parse"));
    assert!(doc.to_string() == text, "private export: round trip differs");
    let cal = &doc.components[0];
    let header: Vec<Item> = cal.items.iter().filter(|i| matches!(i, Item::Property(_))).cloned().collect();
    let zones: Vec<Item> =
        cal.items.iter().filter(|i| matches!(i, Item::Component(c) if c.name == "VTIMEZONE")).cloned().collect();
    let mut by_uid: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    let mut order: Vec<String> = Vec::new();
    for item in &cal.items {
        if let Item::Component(c) = item
            && c.name != "VTIMEZONE"
        {
            let uid = c.property("UID").map(|l| l.value().to_string()).unwrap_or_default();
            if !by_uid.contains_key(&uid) {
                order.push(uid.clone());
            }
            by_uid.entry(uid).or_default().push(item.clone());
        }
    }
    order
        .into_iter()
        .map(|uid| {
            let mut c = Component { items: header.clone(), ..cal.clone() };
            c.items.extend(zones.iter().cloned());
            c.items.extend(by_uid.remove(&uid).unwrap_or_default());
            c.to_string()
        })
        .collect()
}

#[test]
fn private_calendar_corpus() {
    let files = private_files("ics");
    if files.is_empty() {
        eprintln!("SKIP: no fixtures/private/*.ics");
        return;
    }
    let mut t = Tally::default();
    let mut i = 0;
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_else(|_| panic!("private file is not UTF-8"));
        for obj in objects_from_export(&text) {
            check_calendar_object(i, &obj, &mut t);
            i += 1;
        }
    }
    eprintln!("private calendar corpus: {t:?}");
    assert!(t.objects > 0);
}

#[test]
fn private_card_corpus() {
    let files = private_files("vcf");
    if files.is_empty() {
        eprintln!("SKIP: no fixtures/private/*.vcf");
        return;
    }
    let (mut cards, mut with_uid, mut with_name) = (0, 0, 0);
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_else(|_| panic!("private file is not UTF-8"));
        let doc = Document::parse(&text).unwrap_or_else(|_| panic!("private vcf does not parse"));
        assert!(doc.to_string() == text, "private vcf: round trip differs");
        for (i, c) in doc.components.iter().enumerate() {
            let one = c.to_string();
            let info = check_card(i, &one);
            cards += 1;
            with_uid += usize::from(info.uid.is_some());
            with_name += usize::from(info.formatted_name.is_some());
        }
    }
    eprintln!("private card corpus: cards={cards} with_uid={with_uid} with_fn={with_name}");
    assert!(cards > 0);
}

/// Every property line's exact source, in every component.
fn line_sources(text: &str) -> Vec<String> {
    fn walk(c: &Component, out: &mut Vec<String>) {
        for item in &c.items {
            match item {
                Item::Property(l) => out.push(l.source()),
                Item::Component(inner) => walk(inner, out),
            }
        }
    }
    let doc = Document::parse(text).unwrap_or_else(|_| panic!("an object does not parse"));
    let mut out = Vec::new();
    for c in &doc.components {
        walk(c, &mut out);
    }
    out
}

/// How many of `before`'s lines (not named in `except`) are missing from
/// `after`, counting duplicates.
fn missing_lines(before: &str, after: &str, except: &[&str]) -> usize {
    let mut pool = line_sources(after);
    let mut missing = 0;
    for s in line_sources(before) {
        let name = calzone_core::lines::Line::from_source(&s).map(|l| l.name()).unwrap_or_default();
        if except.contains(&name.as_str()) {
            continue;
        }
        match pool.iter().position(|p| *p == s) {
            Some(i) => {
                pool.swap_remove(i);
            }
            None => missing += 1,
        }
    }
    missing
}

fn occurrence_starts(text: &str, from: NaiveDateTime, to: NaiveDateTime) -> Vec<NaiveDateTime> {
    let tz = chrono_tz::Europe::Amsterdam;
    let mut v: Vec<NaiveDateTime> = recur::expand(text, from, to, tz)
        .map(|e| e.occurrences.iter().map(|o| o.start.instant(tz)).collect())
        .unwrap_or_default();
    v.sort();
    v
}

/// Phase 2 step 1 exit check: edits over the household's events leave every
/// line they do not touch byte for byte, and recurring edits neither lose
/// nor double an occurrence. CONFIDENTIAL input: counts only.
#[test]
fn private_corpus_edits_are_lossless() {
    use calzone_core::edit::{self, EventChange, Scope};
    use calzone_core::moment::Key;

    let files = private_files("ics");
    if files.is_empty() {
        eprintln!("SKIP: no fixtures/private/*.ics");
        return;
    }
    let now = dt("2026-11-06 12:00");
    let stamps = ["DTSTAMP", "LAST-MODIFIED"];
    let (mut events, mut recurring, mut splits) = (0, 0, 0);
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_else(|_| panic!("private file is not UTF-8"));
        for (i, obj) in objects_from_export(&text).iter().enumerate() {
            if !obj.contains("BEGIN:VEVENT") {
                continue;
            }
            events += 1;
            let noop = edit::edit(obj, None, Scope::All, &EventChange::default(), now, "n")
                .unwrap_or_else(|e| panic!("event {i}: no-op edit refused: {e}"))
                .updated
                .unwrap();
            assert_eq!(missing_lines(obj, &noop, &stamps), 0, "event {i}: a no-op edit changed a line");
            assert_eq!(
                line_sources(&noop).len(),
                line_sources(obj).len() + stamp_additions(obj),
                "event {i}: a no-op edit added lines"
            );

            let titled = edit::edit(
                obj,
                None,
                Scope::All,
                &EventChange { summary: Some("Renamed".into()), ..Default::default() },
                now,
                "n",
            )
            .unwrap_or_else(|e| panic!("event {i}: title edit refused: {e}"))
            .updated
            .unwrap();
            assert_eq!(
                missing_lines(obj, &titled, &["SUMMARY", "DTSTAMP", "LAST-MODIFIED"]),
                0,
                "event {i}: a title edit changed another line"
            );
            let (from, to) = (dt("2000-01-01 00:00"), dt("2030-01-01 00:00"));
            let before = occurrence_starts(obj, from, to);
            assert!(occurrence_starts(&titled, from, to) == before, "event {i}: a title edit moved occurrences");

            // Recurring: the second occurrence, all three scopes.
            if before.len() < 3 {
                continue;
            }
            let tz = chrono_tz::Europe::Amsterdam;
            let exp = recur::expand(obj, from, to, tz).unwrap();
            let Some(rid) = exp.occurrences.iter().filter_map(|o| o.recurrence_id).nth(1) else { continue };
            recurring += 1;
            let key = match rid {
                recur::When::Date(d) => Key::Date(d),
                recur::When::Utc(t) | recur::When::Floating(t) => Key::Time(t),
                recur::When::Zoned { local, .. } => Key::Time(local),
            };
            let change = EventChange { summary: Some("Only this".into()), ..Default::default() };
            let this = edit::edit(obj, Some(key), Scope::This, &change, now, "n")
                .unwrap_or_else(|e| panic!("event {i}: only-this edit refused: {e}"))
                .updated
                .unwrap();
            assert_eq!(missing_lines(obj, &this, &[]), 0, "event {i}: only-this changed an existing line");
            assert!(occurrence_starts(&this, from, to) == before, "event {i}: only-this moved occurrences");
            let gone = edit::delete(obj, Some(key), Scope::This, now).unwrap().updated.unwrap();
            assert_eq!(occurrence_starts(&gone, from, to).len(), before.len() - 1, "event {i}: delete-this");
            let split = edit::edit(
                obj,
                Some(key),
                Scope::Following,
                &EventChange { summary: Some("From here".into()), ..Default::default() },
                now,
                "n2",
            )
            .unwrap_or_else(|e| panic!("event {i}: following edit refused: {e}"));
            if let Some(new) = split.created {
                splits += 1;
                let mut both = occurrence_starts(&split.updated.unwrap(), from, to);
                both.extend(occurrence_starts(&new, from, to));
                both.sort();
                assert!(
                    both == before,
                    "event {i}: a split lost or doubled occurrences ({} vs {})",
                    both.len(),
                    before.len()
                );
            }
        }
    }
    eprintln!("household edits: events={events} recurring={recurring} splits={splits}");
    assert!(events > 0);
}

/// Lines a no-op edit may add: DTSTAMP or LAST-MODIFIED where the master
/// had none.
fn stamp_additions(obj: &str) -> usize {
    let doc = Document::parse(obj).unwrap();
    let master = doc.components[0]
        .components()
        .find(|c| c.name == "VEVENT" && c.property("RECURRENCE-ID").is_none())
        .or_else(|| doc.components[0].components().find(|c| c.name == "VEVENT"))
        .unwrap();
    ["DTSTAMP", "LAST-MODIFIED"].iter().filter(|n| master.property(n).is_none()).count()
}

/// Phase 2 step 4 check: every household card reads, a no-op edit and an
/// edit that sends every current value back change nothing, and a note
/// edit keeps every other line. CONFIDENTIAL input: counts only.
#[test]
fn private_corpus_card_edits_are_lossless() {
    use calzone_core::card::{self, CardChange};

    let files = private_files("vcf");
    if files.is_empty() {
        eprintln!("SKIP: no fixtures/private/*.vcf");
        return;
    }
    let now = dt("2026-11-06 12:00");
    let (mut cards, mut with_phone, mut with_photo) = (0, 0, 0);
    for f in files {
        let text = std::fs::read_to_string(&f).unwrap_or_else(|_| panic!("private file is not UTF-8"));
        let doc = Document::parse(&text).unwrap_or_else(|_| panic!("private vcf does not parse"));
        for (i, c) in doc.components.iter().enumerate() {
            let src = c.to_string();
            cards += 1;
            let v = card::read(&src).unwrap_or_else(|e| panic!("card {i}: does not read: {e}"));
            with_phone += usize::from(!v.phones.is_empty());
            with_photo += usize::from(v.photo.is_some());
            assert!(
                card::apply(&src, &CardChange::default(), now).unwrap() == src,
                "card {i}: a no-op edit changed it"
            );
            let same = CardChange {
                given: Some(v.given.clone()),
                family: Some(v.family.clone()),
                org: Some(v.org.clone()),
                phones: Some(v.phones.clone()),
                emails: Some(v.emails.clone()),
                addresses: Some(v.addresses.clone()),
                note: Some(v.note.clone()),
                ..Default::default()
            };
            match card::apply(&src, &same, now) {
                Ok(out) => assert!(out == src, "card {i}: sending its own values back changed it"),
                Err(card::CardError::NoName) => {
                    assert!(v.name.trim().is_empty(), "card {i}: refused although it has a name")
                }
                Err(e) => panic!("card {i}: {e}"),
            }
            if let Ok(out) = card::apply(&src, &CardChange { note: Some("Edited".into()), ..Default::default() }, now) {
                assert_eq!(
                    missing_lines(&src, &out, &["NOTE", "REV"]),
                    0,
                    "card {i}: a note edit changed another line"
                );
            }
        }
    }
    eprintln!("household cards: {cards} edited losslessly ({with_phone} with phones, {with_photo} with photos)");
    assert!(cards > 0);
}
