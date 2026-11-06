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
