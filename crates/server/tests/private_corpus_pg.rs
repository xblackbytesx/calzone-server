//! The household corpus through the real store: every event and card is
//! written and read back byte for byte, alarms included (they travel through
//! the owner's layer). CONFIDENTIAL input: counts only, and no assertion
//! message carries anything from the data. SKIP without a database or
//! without `fixtures/private/`.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use calzone_core::lines::{Component, Document, Item};
use calzone_server::domain::{CollectionKind, NewCollection, Precondition, collections, objects, principals};

fn private_files(ext: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/private");
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut v: Vec<PathBuf> =
        rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.extension().is_some_and(|e| e == ext)).collect();
    v.sort();
    v
}

/// One object per UID, as a CalDAV server stores a merged export.
fn objects_from_export(text: &str) -> Vec<String> {
    let doc = Document::parse(text).unwrap_or_else(|_| panic!("private export does not parse"));
    let cal = &doc.components[0];
    let header: Vec<Item> = cal.items.iter().filter(|i| matches!(i, Item::Property(_))).cloned().collect();
    let zones: Vec<Item> =
        cal.items.iter().filter(|i| matches!(i, Item::Component(c) if c.name == "VTIMEZONE")).cloned().collect();
    let mut by_uid: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    let mut order = Vec::new();
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

/// Events (one object per UID) and cards from the private exports; None to skip.
fn corpus() -> Option<(Vec<String>, Vec<String>)> {
    let (ics, vcf) = (private_files("ics"), private_files("vcf"));
    if ics.is_empty() && vcf.is_empty() {
        eprintln!("SKIP: no fixtures/private/");
        return None;
    }
    let mut events = Vec::new();
    for f in ics {
        let text = std::fs::read_to_string(f).unwrap_or_else(|_| panic!("private file is not UTF-8"));
        events.extend(objects_from_export(&text));
    }
    let mut cards = Vec::new();
    for f in vcf {
        let text = std::fs::read_to_string(f).unwrap_or_else(|_| panic!("private file is not UTF-8"));
        let doc = Document::parse(&text).unwrap_or_else(|_| panic!("private vcf does not parse"));
        cards.extend(doc.components.iter().map(|c| c.to_string()));
    }
    Some((events, cards))
}

/// The same corpus through the DAV door: PUT every object, then read all of
/// it back with one sync-collection REPORT each, through our own XML.
#[tokio::test]
async fn the_household_corpus_survives_the_dav_door() {
    use axum::http::StatusCode;
    use calzone_dav::xml::{self, CALDAV, CARDDAV, DAV};

    let Some((events, cards)) = corpus() else { return };
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "owner", "").await.unwrap();
    let (_, pw) = calzone_server::domain::app_passwords::create(p, me, "test", None).await.unwrap();
    let new =
        |kind, slug| NewCollection { kind, slug, display_name: "", description: "", color: None, components: &[] };
    collections::create(p, me, new(CollectionKind::Calendar, "calendar")).await.unwrap();
    collections::create(p, me, new(CollectionKind::AddressBook, "contacts")).await.unwrap();
    let app = db.app();
    let auth = Some(("owner", pw.as_str()));

    for (base, items, ext, data) in [
        ("/dav/calendars/owner/calendar/", &events, "ics", (CALDAV, "calendar-data")),
        ("/dav/addressbooks/owner/contacts/", &cards, "vcf", (CARDDAV, "address-data")),
    ] {
        for (i, body) in items.iter().enumerate() {
            let r =
                common::send(&app, "PUT", &format!("{base}o{i}.{ext}"), auth, &[("if-none-match", "*")], body).await;
            assert!(r.status == StatusCode::CREATED, "{ext} {i}: PUT refused with {}", r.status);
        }
        let prefix = if ext == "ics" { "c" } else { "card" };
        let report = format!(
            r#"<d:sync-collection xmlns:d="DAV:" xmlns:c="{CALDAV}" xmlns:card="{CARDDAV}"><d:sync-token/><d:sync-level>1</d:sync-level><d:prop><d:getetag/><{prefix}:{}/></d:prop></d:sync-collection>"#,
            data.1
        );
        let r = common::send(&app, "REPORT", base, auth, &[], &report).await;
        assert!(r.status == StatusCode::MULTI_STATUS, "{ext}: REPORT failed with {}", r.status);
        let root = xml::parse(r.body.as_bytes()).unwrap_or_else(|_| panic!("{ext}: multistatus does not parse"));
        let mut seen = 0;
        for resp in root.children_named(DAV, "response") {
            let href = &resp.child(DAV, "href").unwrap().text;
            let i: usize =
                href.rsplit('/').next().unwrap().trim_start_matches('o').split('.').next().unwrap().parse().unwrap();
            let got = resp
                .children_named(DAV, "propstat")
                .find_map(|ps| ps.child(DAV, "prop")?.child(data.0, data.1))
                .map(|e| e.text.as_str());
            assert!(got == Some(items[i].as_str()), "{ext} {i}: read back through DAV differs");
            seen += 1;
        }
        assert_eq!(seen, items.len(), "{ext}: every object comes back");
    }
    eprintln!("household corpus through the DAV door: events={} cards={}", events.len(), cards.len());
    db.drop().await;
}

#[tokio::test]
async fn the_household_corpus_survives_the_store() {
    let Some((events, cards)) = corpus() else { return };
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "owner", "").await.unwrap();
    let new =
        |kind, slug| NewCollection { kind, slug, display_name: "", description: "", color: None, components: &[] };
    let cal = collections::create(p, me, new(CollectionKind::Calendar, "calendar")).await.unwrap().id;
    let book = collections::create(p, me, new(CollectionKind::AddressBook, "contacts")).await.unwrap().id;

    for (i, body) in events.iter().enumerate() {
        objects::put(p, me, cal, &format!("e{i}.ics"), body, Precondition::IfNoneMatchAny)
            .await
            .unwrap_or_else(|e| panic!("event {i}: refused: {e}"));
    }
    for (i, body) in cards.iter().enumerate() {
        objects::put(p, me, book, &format!("c{i}.vcf"), body, Precondition::IfNoneMatchAny)
            .await
            .unwrap_or_else(|e| panic!("card {i}: refused: {e}"));
    }
    for (i, body) in events.iter().enumerate() {
        let got = objects::get(p, me, cal, &format!("e{i}.ics")).await.unwrap();
        assert!(got.body == *body, "event {i}: read back differs");
    }
    for (i, body) in cards.iter().enumerate() {
        let got = objects::get(p, me, book, &format!("c{i}.vcf")).await.unwrap();
        assert!(got.body == *body, "card {i}: read back differs");
    }
    let alarms_in_shared: i64 =
        sqlx::query_scalar("SELECT count(*) FROM objects WHERE body LIKE '%BEGIN:VALARM%'").fetch_one(p).await.unwrap();
    assert_eq!(alarms_in_shared, 0, "no alarm stays in a shared body");
    let layers: i64 = sqlx::query_scalar("SELECT count(*) FROM user_layers").fetch_one(p).await.unwrap();
    let full = objects::changes_since(p, me, cal, None).await.unwrap();
    assert_eq!(full.changed.len(), events.len());
    eprintln!("household corpus through the store: events={} cards={} layers={layers}", events.len(), cards.len());
    db.drop().await;
}

/// Components (VTIMEZONEs aside), unfolded, sorted: what an import and an
/// export must agree on.
fn event_components(objects: &[String]) -> Vec<String> {
    let mut v: Vec<String> = objects
        .iter()
        .flat_map(|o| {
            let d = Document::parse(o).unwrap_or_else(|_| panic!("an object does not parse"));
            d.components[0]
                .components()
                .filter(|c| c.name != "VTIMEZONE")
                .map(|c| c.to_string().replace("\r\n ", "").replace("\r\n\t", "").replace("\r\n", "\n"))
                .collect::<Vec<_>>()
        })
        .collect();
    v.sort();
    v
}

/// The household's Baikal export files through the file importer: every
/// event and card lands, a second run changes nothing, and the export gives
/// back the same events and the same cards byte for byte.
#[tokio::test]
async fn the_household_export_files_import_converge_and_export() {
    use calzone_server::domain::imports::{self, Status};
    use calzone_server::import::{Run, Target, Upload};

    let (ics, vcf) = (private_files("ics"), private_files("vcf"));
    if ics.is_empty() && vcf.is_empty() {
        eprintln!("SKIP: no fixtures/private/");
        return;
    }
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "owner", "").await.unwrap();
    let uploads: Vec<Upload> = ics
        .iter()
        .chain(vcf.iter())
        .enumerate()
        .map(|(i, f)| Upload {
            // A neutral name: the real one could say something about the data.
            filename: format!("file{i}.{}", f.extension().unwrap().to_str().unwrap()),
            bytes: std::fs::read(f).unwrap_or_else(|_| panic!("private file unreadable")),
        })
        .collect();

    let run = |uploads: Vec<Upload>| async {
        let run = Run::begin(p, me, me, "corpus").await.unwrap();
        let id = run.files(uploads, Target::ByName).await;
        imports::get(p, me, id).await.unwrap()
    };
    let first = run(uploads.clone()).await;
    assert!(first.status == Status::Done, "import failed");
    let t = first.totals();
    assert_eq!(t.failed, 0, "objects not imported: {}", t.failed);
    let (events, cards) = corpus().unwrap();
    assert_eq!(
        t.created as usize,
        events.len() + cards.len(),
        "created {} of {}",
        t.created,
        events.len() + cards.len()
    );

    let again = run(uploads).await;
    let t2 = again.totals();
    assert_eq!(
        (t2.created, t2.updated, t2.failed),
        (0, 0, 0),
        "a second run changed {} objects",
        t2.created + t2.updated
    );
    assert_eq!(t2.unchanged, t.created);

    for c in &first.summary {
        let id = c.collection_id.unwrap();
        let (_, _, text) = calzone_server::export::collection(p, me, id).await.unwrap();
        let col = collections::get(p, me, id).await.unwrap();
        if col.kind == CollectionKind::Calendar {
            let back = calzone_core::import::split_calendar(&text).unwrap_or_else(|_| panic!("export does not split"));
            let (a, b) = (event_components(&events), event_components(&back.objects));
            let same = a.iter().zip(&b).filter(|(x, y)| x == y).count();
            assert!(a == b, "export differs: {same} of {} components equal, {} exported", a.len(), b.len());
        } else {
            let mut back = calzone_core::import::split_cards(&text).unwrap_or_else(|_| panic!("export does not split"));
            let mut want = cards.clone();
            back.sort();
            want.sort();
            let same = want.iter().zip(&back).filter(|(x, y)| x == y).count();
            assert!(want == back, "cards differ: {same} of {} equal", want.len());
        }
    }
    eprintln!(
        "household files through import and export: events={} cards={} unchanged on re-run={}",
        events.len(),
        cards.len(),
        t2.unchanged
    );
    db.drop().await;
}
