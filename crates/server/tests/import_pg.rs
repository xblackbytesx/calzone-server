//! Imports and exports against a real Postgres (decision 30): files
//! (Takeout-shaped `.ics`, `.vcf`, a zip) converge on a re-run, keep what
//! they cannot store, and put alarms in the importer's own layer; exports
//! give back what went in; the Baikal importer reads a real CalDAV/CardDAV
//! server (our own door over TCP), with Basic and with Digest auth.

mod common;

use std::sync::OnceLock;

use axum::extract::Request;
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use calzone_core::import::split_calendar;
use calzone_core::lines::Document;
use calzone_server::domain::imports::{self, Status};
use calzone_server::domain::{
    Access, CollectionKind, NewCollection, Precondition, app_passwords, collections, objects, principals,
};
use calzone_server::import::{Run, Target, Upload, baikal};
use common::Db;
use uuid::Uuid;

const ZONE: &str = "BEGIN:VTIMEZONE\r\nTZID:Europe/Amsterdam\r\nBEGIN:STANDARD\r\nDTSTART:19701025T030000\r\nRRULE:FREQ=YEARLY;BYMONTH=10;BYDAY=-1SU\r\nTZOFFSETFROM:+0200\r\nTZOFFSETTO:+0100\r\nEND:STANDARD\r\nBEGIN:DAYLIGHT\r\nDTSTART:19700329T020000\r\nRRULE:FREQ=YEARLY;BYMONTH=3;BYDAY=-1SU\r\nTZOFFSETFROM:+0100\r\nTZOFFSETTO:+0200\r\nEND:DAYLIGHT\r\nEND:VTIMEZONE\r\n";

/// A Google Takeout calendar: METHOD, a recurring event with an override
/// (bumped SEQUENCE), an event with an alarm, and a task without UID.
fn takeout(swim: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nPRODID:-//Google Inc//Google Calendar 70.9054//EN\r\nVERSION:2.0\r\nCALSCALE:GREGORIAN\r\n\
         METHOD:PUBLISH\r\nX-WR-CALNAME:Family\r\nX-WR-TIMEZONE:Europe/Amsterdam\r\n{ZONE}\
         BEGIN:VEVENT\r\nDTSTART;TZID=Europe/Amsterdam:20260105T090000\r\nDTEND;TZID=Europe/Amsterdam:20260105T100000\r\n\
         RRULE:FREQ=WEEKLY;COUNT=10\r\nDTSTAMP:20260101T000000Z\r\nUID:swim@google.com\r\nSUMMARY:{swim}\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nDTSTART:20260301T100000Z\r\nDTEND:20260301T110000Z\r\nDTSTAMP:20260101T000000Z\r\n\
         UID:dentist@google.com\r\nSUMMARY:Dentist\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT30M\r\n\
         DESCRIPTION:Reminder\r\nEND:VALARM\r\nEND:VEVENT\r\n\
         BEGIN:VEVENT\r\nDTSTART;TZID=Europe/Amsterdam:20260113T100000\r\nDTEND;TZID=Europe/Amsterdam:20260113T110000\r\n\
         RECURRENCE-ID;TZID=Europe/Amsterdam:20260112T090000\r\nDTSTAMP:20260101T000000Z\r\nUID:swim@google.com\r\n\
         SEQUENCE:2\r\nSUMMARY:Swim (moved)\r\nEND:VEVENT\r\n\
         BEGIN:VTODO\r\nSUMMARY:no uid\r\nEND:VTODO\r\n\
         END:VCALENDAR\r\n"
    )
}

/// Two cards, a second card with the first one's UID, and a card without UID.
const CARDS: &str = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:aaaa\r\nFN:Ada\r\nEMAIL:ada@example.org\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nUID:bbbb\r\nFN:Bea\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:4.0\r\nUID:urn:uuid:aaaa\r\nFN:Ada again\r\nEND:VCARD\r\n\
BEGIN:VCARD\r\nVERSION:3.0\r\nFN:No Uid\r\nEND:VCARD\r\n";

fn files(swim: &str) -> Vec<Upload> {
    vec![
        Upload { filename: "family.ics".into(), bytes: takeout(swim).into_bytes() },
        Upload { filename: "contacts.vcf".into(), bytes: CARDS.as_bytes().to_vec() },
    ]
}

async fn import(db: &Db, into: Uuid, uploads: Vec<Upload>, target: Target) -> imports::Import {
    let run = Run::begin(&db.pool, into, into, "test").await.unwrap();
    let id = run.files(uploads, target).await;
    imports::get(&db.pool, into, id).await.unwrap()
}

/// Components without the calendar wrapper, unfolded, per object.
fn components(objects: &[String]) -> Vec<String> {
    let mut v: Vec<String> = objects
        .iter()
        .map(|o| {
            let d = Document::parse(o).unwrap();
            d.components[0]
                .components()
                .filter(|c| c.name != "VTIMEZONE")
                .map(|c| c.to_string().replace("\r\n ", "").replace("\r\n\t", ""))
                .collect::<String>()
        })
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn file_imports_converge_keep_failures_and_layer_alarms() {
    let Some(db) = Db::new().await else { return };
    let p = &db.pool;
    let anna = principals::create_user(p, "anna", "").await.unwrap();
    let ben = principals::create_user(p, "ben", "").await.unwrap();

    let first = import(&db, anna, files("Swim"), Target::ByName).await;
    assert_eq!(first.status, Status::Done, "{}", first.message);
    let [cal, book] = first.summary.as_slice() else { panic!("two collections: {:?}", first.summary) };
    assert_eq!((cal.name.as_str(), cal.created_collection), ("Family", true));
    assert_eq!((cal.created, cal.updated, cal.unchanged, cal.failed), (2, 0, 0, 1));
    assert_eq!((book.name.as_str(), book.created_collection), ("contacts", true));
    assert_eq!((book.created, book.failed), (3, 1), "the duplicate UID is kept aside");

    let fails = imports::failures(p, anna, first.id).await.unwrap();
    assert_eq!(fails.len(), 2);
    assert!(fails[0].2.contains("no UID") || fails[0].2.contains("valid"), "{}", fails[0].2);
    let raw = imports::failure_raw(p, anna, first.id, fails[0].0).await.unwrap();
    assert!(String::from_utf8(raw).unwrap().contains("SUMMARY:no uid"), "the refused object is kept whole");
    let raw = imports::failure_raw(p, anna, first.id, fails[1].0).await.unwrap();
    assert!(String::from_utf8(raw).unwrap().contains("FN:Ada again"));
    assert!(imports::failures(p, ben, first.id).await.is_err(), "reports are private");

    // The same files again: nothing changes, not even the sync token.
    let cal_id = cal.collection_id.unwrap();
    let book_id = book.collection_id.unwrap();
    let seq = collections::get(p, anna, cal_id).await.unwrap().sequence;
    let again = import(&db, anna, files("Swim"), Target::ByName).await;
    let [cal2, book2] = again.summary.as_slice() else { panic!("{:?}", again.summary) };
    assert_eq!((cal2.collection_id, cal2.created_collection), (Some(cal_id), false));
    assert_eq!((cal2.created, cal2.updated, cal2.unchanged, cal2.failed), (0, 0, 2, 1));
    assert_eq!((book2.collection_id, book2.created, book2.updated, book2.unchanged), (Some(book_id), 0, 0, 3));
    assert_eq!(collections::get(p, anna, cal_id).await.unwrap().sequence, seq, "a re-run writes nothing");
    assert_eq!(collections::visible(p, anna, CollectionKind::Calendar).await.unwrap().len(), 1);

    // A changed event is updated in place.
    let changed = import(&db, anna, files("Swimming"), Target::Existing(cal_id)).await;
    assert_eq!((changed.summary[0].updated, changed.summary[0].unchanged), (1, 1));
    assert_eq!(changed.summary[1].failed, 1, "the address book file does not fit a calendar");
    let swim = objects::get(p, anna, cal_id, "swim@google.com.ics").await.unwrap();
    assert!(swim.body.contains("SUMMARY:Swimming") && swim.body.contains("Swim (moved)"));
    assert!(!swim.body.contains("METHOD"), "METHOD is dropped from stored objects");

    // Alarms belong to the importer: ben sees the event, not anna's reminder.
    collections::set_grant(p, anna, cal_id, ben, Some(Access::Read)).await.unwrap();
    let dentist = "dentist@google.com.ics";
    assert!(objects::get(p, anna, cal_id, dentist).await.unwrap().body.contains("BEGIN:VALARM"));
    assert!(!objects::get(p, ben, cal_id, dentist).await.unwrap().body.contains("VALARM"));

    // Read-only targets are refused as a whole.
    let ro = import(&db, ben, files("x"), Target::Existing(cal_id)).await;
    assert_eq!(ro.status, Status::Failed);
    assert!(ro.message.contains("read-only"));
    db.drop().await;
}

#[tokio::test]
async fn zips_bad_bytes_and_split_folds() {
    use std::io::Write;
    let Some(db) = Db::new().await else { return };
    let p = &db.pool;
    let anna = principals::create_user(p, "anna", "").await.unwrap();

    // A Takeout zip: one .ics per calendar, plus things to ignore.
    let mut buf = std::io::Cursor::new(Vec::new());
    {
        let mut z = zip::ZipWriter::new(&mut buf);
        let o = zip::write::SimpleFileOptions::default();
        z.start_file("Takeout/Calendar/Family.ics", o).unwrap();
        z.write_all(takeout("Swim").as_bytes()).unwrap();
        z.start_file("Takeout/Calendar/Work.ics", o).unwrap();
        z.write_all(takeout("Swim").replace("Family", "Work").replace("@google", "@work").as_bytes()).unwrap();
        z.start_file("Takeout/archive_browser.html", o).unwrap();
        z.write_all(b"<html></html>").unwrap();
        z.finish().unwrap();
    }
    // A byte-counting writer folded inside "é" (C3 A9).
    let split = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:cafe\r\nFN:Caf\xC3\r\n \xA9\r\nEND:VCARD\r\n".to_vec();
    let latin1 = b"BEGIN:VCARD\r\nVERSION:3.0\r\nUID:x\r\nFN:Caf\xE9\r\nEND:VCARD\r\n".to_vec();
    let uploads = vec![
        Upload { filename: "takeout.zip".into(), bytes: buf.into_inner() },
        Upload { filename: "folded.vcf".into(), bytes: split },
        Upload { filename: "latin1.vcf".into(), bytes: latin1.clone() },
        Upload { filename: "notes.txt".into(), bytes: b"hello".to_vec() },
    ];
    let r = import(&db, anna, uploads, Target::ByName).await;
    assert_eq!(r.status, Status::Done);
    let names: Vec<&str> = r.summary.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, ["Family", "Work", "folded", "latin1.vcf", "notes.txt"]);
    assert_eq!((r.summary[0].created, r.summary[1].created), (2, 2));
    assert_eq!(r.summary[2].created, 1);
    let folded = r.summary[2].collection_id.unwrap();
    let card = objects::get(p, anna, folded, "cafe.vcf").await.unwrap();
    assert!(card.body.contains("FN:Café"), "the split fold was joined");
    assert_eq!((r.summary[3].failed, r.summary[4].failed), (1, 1));
    let fails = imports::failures(p, anna, r.id).await.unwrap();
    assert_eq!(fails.len(), 4, "a UID-less task per calendar, the Latin-1 card, the text file");
    let bad = fails.iter().find(|f| f.1 == "latin1.vcf").unwrap();
    assert!(bad.2.contains("not UTF-8"), "{}", bad.2);
    assert_eq!(imports::failure_raw(p, anna, r.id, bad.0).await.unwrap(), latin1, "kept byte for byte");
    db.drop().await;
}

#[tokio::test]
async fn exports_give_back_what_went_in() {
    let Some(db) = Db::new().await else { return };
    let p = &db.pool;
    let anna = principals::create_user(p, "anna", "").await.unwrap();
    let ben = principals::create_user(p, "ben", "").await.unwrap();
    let r = import(&db, anna, files("Swim"), Target::ByName).await;
    let cal = r.summary[0].collection_id.unwrap();

    let (name, ct, text) = calzone_server::export::collection(p, anna, cal).await.unwrap();
    assert_eq!((name.as_str(), ct), ("family.ics", "text/calendar; charset=utf-8"));
    assert!(text.contains("X-WR-CALNAME:Family") && text.contains("BEGIN:VALARM"));
    assert_eq!(text.matches("BEGIN:VTIMEZONE").count(), 1);
    // Every stored component comes back (the UID-less task was never stored).
    let source = split_calendar(&takeout("Swim")).unwrap().objects;
    let stored: Vec<String> = source.iter().filter(|o| !o.contains("no uid")).cloned().collect();
    assert_eq!(components(&split_calendar(&text).unwrap().objects), components(&stored));

    // And the export imports into a fresh account as the same thing.
    let carol = principals::create_user(p, "carol", "").await.unwrap();
    let back =
        import(&db, carol, vec![Upload { filename: "family.ics".into(), bytes: text.into_bytes() }], Target::ByName)
            .await;
    assert_eq!((back.summary[0].created, back.summary[0].failed), (2, 0));

    // The zip: own collections at the top, shared ones under shared/.
    let theirs = collections::create(
        p,
        ben,
        NewCollection {
            kind: CollectionKind::Calendar,
            slug: "work",
            display_name: "Work",
            description: "",
            color: None,
            components: &[],
        },
    )
    .await
    .unwrap();
    collections::set_grant(p, ben, theirs.id, anna, Some(Access::Read)).await.unwrap();
    let zip = calzone_server::export::everything(p, anna).await.unwrap();
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(zip)).unwrap();
    let mut names: Vec<String> = (0..z.len()).map(|i| z.by_index(i).unwrap().name().to_string()).collect();
    names.sort();
    assert_eq!(names, ["contacts.vcf", "family.ics", "shared/ben/work.ics"]);
    let mut vcf = String::new();
    std::io::Read::read_to_string(&mut z.by_name("contacts.vcf").unwrap(), &mut vcf).unwrap();
    assert_eq!(vcf.matches("BEGIN:VCARD").count(), 3);
    db.drop().await;
}

/// A source account on a real server: a calendar with colour and
/// components, events with an alarm and an override, and cards (one with a
/// space in its resource name). Returns (source id, app password).
async fn source_account(db: &Db) -> (Uuid, String) {
    let p = &db.pool;
    let src = principals::create_user(p, "src", "").await.unwrap();
    let (_, pw) = app_passwords::create(p, src, "import", None).await.unwrap();
    let fam = collections::create(
        p,
        src,
        NewCollection {
            kind: CollectionKind::Calendar,
            slug: "family",
            display_name: "Family",
            description: "Home things",
            color: Some("#112233"),
            components: &["VEVENT"],
        },
    )
    .await
    .unwrap();
    for o in split_calendar(&takeout("Swim")).unwrap().objects.iter().filter(|o| !o.contains("no uid")) {
        let name = calzone_server::import::resource_name(o, "ics");
        objects::put(p, src, fam.id, &name, o, Precondition::None).await.unwrap();
    }
    let book = collections::create(
        p,
        src,
        NewCollection {
            kind: CollectionKind::AddressBook,
            slug: "default",
            display_name: "Contacts",
            description: "",
            color: None,
            components: &[],
        },
    )
    .await
    .unwrap();
    objects::put(
        p,
        src,
        book.id,
        "a b.vcf",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ab\r\nFN:A B\r\nEND:VCARD\r\n",
        Precondition::None,
    )
    .await
    .unwrap();
    objects::put(
        p,
        src,
        book.id,
        "c.vcf",
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:c\r\nFN:Caf\u{e9}\r\nEND:VCARD\r\n",
        Precondition::None,
    )
    .await
    .unwrap();
    (src, pw)
}

async fn bodies(db: &Db, actor: Uuid, collection: Uuid) -> Vec<(String, String)> {
    objects::all(&db.pool, actor, collection).await.unwrap().into_iter().map(|s| (s.meta.name, s.body)).collect()
}

async fn baikal_import(db: &Db, into: Uuid, url: &str, user: &str, password: &str) -> imports::Import {
    let run = Run::begin(&db.pool, into, into, "test").await.unwrap();
    let src = baikal::Source { url: url.into(), user: user.into(), password: password.into() };
    let id = run.baikal(src).await;
    imports::get(&db.pool, into, id).await.unwrap()
}

#[tokio::test]
async fn baikal_import_reads_a_dav_server_and_converges() {
    let Some(db) = Db::new().await else { return };
    let p = &db.pool;
    let (src, pw) = source_account(&db).await;
    let dst = principals::create_user(p, "dst", "").await.unwrap();
    let base = common::serve(db.app()).await;
    // Start from the bare server address, as people will type it.
    let r = baikal_import(&db, dst, &format!("{base}/"), "src", &pw).await;
    assert_eq!(r.status, Status::Done, "{}", r.message);
    let [cal, book] = r.summary.as_slice() else { panic!("{:?}", r.summary) };
    assert_eq!((cal.name.as_str(), cal.created_collection, cal.created, cal.failed), ("Family", true, 2, 0));
    assert_eq!((book.name.as_str(), book.created, book.failed), ("Contacts", 2, 0));

    let mine = collections::get(p, dst, cal.collection_id.unwrap()).await.unwrap();
    assert_eq!(
        (mine.slug.as_str(), mine.color.as_deref(), mine.description.as_str()),
        ("family", Some("#112233"), "Home things")
    );
    assert_eq!(mine.components, ["VEVENT"]);
    let theirs = collections::by_path(p, src, "src", "family").await.unwrap();
    assert_eq!(bodies(&db, dst, mine.id).await, bodies(&db, src, theirs.id).await, "byte for byte, alarms included");
    let src_book = collections::by_path(p, src, "src", "default").await.unwrap();
    assert_eq!(bodies(&db, dst, book.collection_id.unwrap()).await, bodies(&db, src, src_book.id).await);
    assert!(bodies(&db, dst, book.collection_id.unwrap()).await.iter().any(|(n, _)| n == "a b.vcf"));

    // Again: nothing changes.
    let seq = mine.sequence;
    let again = baikal_import(&db, dst, &format!("{base}/dav/"), "src", &pw).await;
    assert_eq!(again.status, Status::Done);
    assert_eq!(again.totals().unchanged, 4);
    assert_eq!(again.totals().created + again.totals().updated + again.totals().failed, 0);
    assert_eq!(collections::get(p, dst, mine.id).await.unwrap().sequence, seq);

    // A wrong password stops the import with a plain message.
    let bad = baikal_import(&db, dst, &format!("{base}/dav/"), "src", "wrong").await;
    assert_eq!(bad.status, Status::Failed);
    assert!(bad.message.contains("refused the user name or password"), "{}", bad.message);
    let gone = baikal_import(&db, dst, "http://127.0.0.1:9/dav/", "src", "x").await;
    assert_eq!(gone.status, Status::Failed);
    assert!(gone.message.contains("Could not read from the server"), "{}", gone.message);
    db.drop().await;
}

static APP_PASSWORD: OnceLock<String> = OnceLock::new();
const DIGEST_PASSWORD: &str = "digest secret";

fn md5_hex(s: &str) -> String {
    use md5::Digest as _;
    md5::Md5::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// A Baikal-style front door: Digest only (with a Basic offer that must not
/// be taken). A valid Digest answer is checked here and passed on to the
/// real app as Basic with the source's app password.
async fn digest_gate(mut req: Request, next: Next) -> Response {
    let auth = req.headers().get("authorization").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
    if let Some(rest) = auth.strip_prefix("Digest ") {
        let get = |k: &str| {
            rest.split(", ").find_map(|kv| kv.strip_prefix(&format!("{k}="))).map(|v| v.trim_matches('"').to_string())
        };
        let (uri, nc, cnonce, response) =
            (get("uri").unwrap(), get("nc").unwrap(), get("cnonce").unwrap(), get("response").unwrap());
        let ha1 = md5_hex(&format!("src:BaikalDAV:{DIGEST_PASSWORD}"));
        let ha2 = md5_hex(&format!("{}:{uri}", req.method()));
        let want = md5_hex(&format!("{ha1}:abc123:{nc}:{cnonce}:auth:{ha2}"));
        if want == response && uri == req.uri().path() && get("opaque").as_deref() == Some("xyz") {
            use base64::Engine as _;
            let b = base64::engine::general_purpose::STANDARD.encode(format!("src:{}", APP_PASSWORD.get().unwrap()));
            req.headers_mut().insert("authorization", HeaderValue::from_str(&format!("Basic {b}")).unwrap());
            return next.run(req).await;
        }
    }
    let mut r = StatusCode::UNAUTHORIZED.into_response();
    r.headers_mut().append("www-authenticate", HeaderValue::from_static("Basic realm=\"BaikalDAV\""));
    r.headers_mut().append(
        "www-authenticate",
        HeaderValue::from_static("Digest realm=\"BaikalDAV\",qop=\"auth\",nonce=\"abc123\",opaque=\"xyz\""),
    );
    r
}

#[tokio::test]
async fn baikal_import_speaks_digest() {
    let Some(db) = Db::new().await else { return };
    let p = &db.pool;
    let (_, pw) = source_account(&db).await;
    let _ = APP_PASSWORD.set(pw);
    let dst = principals::create_user(p, "dst", "").await.unwrap();
    let base = common::serve(db.app().layer(middleware::from_fn(digest_gate))).await;
    let r = baikal_import(&db, dst, &format!("{base}/dav/"), "src", DIGEST_PASSWORD).await;
    assert_eq!(r.status, Status::Done, "{}", r.message);
    assert_eq!(r.totals().created, 4);
    let bad = baikal_import(&db, dst, &format!("{base}/dav/"), "src", "nope").await;
    assert_eq!(bad.status, Status::Failed);
    db.drop().await;
}
