//! Contacts clean-up against a real Postgres (decision 56): has/lacks
//! filters, likely duplicates, "Not duplicates" with its Undo, a merge
//! that loses nothing and undoes in one step, bulk delete behind a check
//! page, stale pages refused, read-only books left alone.

mod common;

use axum::Router;
use axum::http::StatusCode;
use calzone_server::domain::{
    Access, CollectionKind, NewCollection, Precondition, accounts, collections, objects, principals,
};
use common::{Db, Reply, send};
use uuid::Uuid;

struct Who {
    app: Router,
    cookie: String,
}

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl Who {
    async fn new(db: &Db, app: &Router, name: &str) -> (Who, Uuid) {
        let id = principals::create_user(&db.pool, name, "").await.unwrap();
        let token = accounts::create_session(&db.pool, id, true).await.unwrap();
        (Who { app: app.clone(), cookie: format!("calzone_session={token}") }, id)
    }

    async fn get(&self, path: &str) -> Reply {
        send(&self.app, "GET", path, None, &[("cookie", &self.cookie), ("accept-language", "en")], "").await
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let body = fields.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&");
        let h = [
            ("cookie", self.cookie.as_str()),
            ("content-type", "application/x-www-form-urlencoded"),
            ("accept-language", "en"),
        ];
        send(&self.app, "POST", path, None, &h, &body).await
    }
}

fn loc(r: &Reply) -> String {
    r.header("location").unwrap_or_default()
}

const ADA: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada1\r\nFN:Ada Lovelace\r\nN:Lovelace;Ada;;;\r\n\
TEL;TYPE=CELL:+44 7700 900001\r\nX-SOCIALPROFILE:ada-social\r\nCATEGORIES:Family\r\nEND:VCARD\r\n";
const ADA2: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:ada2\r\nFN:Lovelace\\, Ada\r\nN:Lovelace;Ada;;;\r\n\
TEL;TYPE=HOME:07700 900001\r\nEMAIL;TYPE=HOME:ada@example.org\r\nitem1.URL:https://ada.example\r\nitem1.X-ABLabel:Blog\r\n\
BDAY:1815-12-10\r\nNOTE:Met at the lab\r\nEND:VCARD\r\n";
const BARE: &str = "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:bare\r\nFN:Old Import\r\nN:Import;Old;;;\r\nEND:VCARD\r\n";
const BEA: &str =
    "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:bea\r\nFN:Bea Smith\r\nN:Smith;Bea;;;\r\nTEL:+31 20 555 0101\r\nEND:VCARD\r\n";
const BEA2: &str =
    "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:bea2\r\nFN:Bea Smith\r\nN:Smith;Bea;;;\r\nORG:Acme\r\nEND:VCARD\r\n";

#[tokio::test]
async fn filter_find_merge_delete_and_undo() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let new = NewCollection {
        kind: CollectionKind::AddressBook,
        slug: "contacts",
        display_name: "Contacts",
        description: "",
        color: None,
        components: &[],
    };
    let b = collections::create(&db.pool, anna_id, new).await.unwrap().id;
    let bs = b.to_string();
    for (n, t) in [("ada1.vcf", ADA), ("ada2.vcf", ADA2), ("bare.vcf", BARE), ("bea.vcf", BEA), ("bea2.vcf", BEA2)] {
        objects::put(&db.pool, anna_id, b, n, t, Precondition::None).await.unwrap();
    }
    let etag = |n: &'static str| {
        let pool = db.pool.clone();
        async move { objects::get(&pool, anna_id, b, n).await.unwrap().meta.etag() }
    };

    // Filters: lacking both a phone and an email; lacking a phone; with an organization.
    let l = anna.get("/contacts?phone=missing&email=missing").await.body;
    assert!(l.contains("Old Import") && l.contains("Bea Smith") && !l.contains("Ada Lovelace"), "{l}");
    assert!(l.contains("Shown: 2") && l.contains("<details class=\"contact-filter\" open>"));
    let l = anna.get("/contacts?org=has").await.body;
    assert!(l.contains("Acme") && !l.contains("Old Import"));
    let l = anna.get("/contacts?q=ada&email=has").await.body;
    assert!(l.contains("Shown: 1"), "search and filter together");
    let l = anna.get(&format!("/contacts?book={bs}&email=missing")).await.body;
    assert!(l.contains("Shown: 4"));
    // Two groups: Ada by phone and name, Bea by name alone.
    let l = anna.get("/contacts").await.body;
    assert!(l.contains("href=\"/contacts/duplicates\"") && l.contains("<span class=\"badge warn\">2</span>"), "{l}");
    let d = anna.get("/contacts/duplicates").await.body;
    assert!(d.contains("Same phone") && d.contains("Same name") && d.contains("Only the name matches"), "{d}");
    assert_eq!(d.matches("class=\"panel dup-group\"").count(), 2);
    assert!(d.find("Ada Lovelace").unwrap() < d.find("Bea Smith").unwrap(), "strong matches first");

    // Not duplicates, its Undo, and again.
    let apart = [("book", bs.as_str()), ("n", "bea.vcf"), ("n", "bea2.vcf")];
    let r = anna.post("/contacts/duplicates/apart", &apart).await;
    assert!(loc(&r).starts_with("/contacts/duplicates?apart="), "{}", r.body);
    let d = anna.get(&loc(&r)).await.body;
    assert!(d.contains("Marked as different people.") && d.matches("class=\"panel dup-group\"").count() == 1);
    let r = anna.post("/contacts/duplicates/together", &apart).await;
    assert_eq!(loc(&r), "/contacts/duplicates");
    assert_eq!(anna.get("/contacts/duplicates").await.body.matches("class=\"panel dup-group\"").count(), 2);
    anna.post("/contacts/duplicates/apart", &apart).await;

    // Select mode: checkboxes, and merge goes to the merge page.
    let l = anna.get("/contacts?select=1").await.body;
    assert!(l.contains(&format!("name=\"pick\" value=\"{bs}/ada1.vcf\"")) && l.contains("value=\"merge\""));
    let r = anna
        .post(
            "/contacts/selected",
            &[("pick", &format!("{bs}/ada1.vcf")), ("pick", &format!("{bs}/ada2.vcf")), ("action", "merge")],
        )
        .await;
    let merge_url = loc(&r);
    assert!(merge_url.starts_with("/contacts/merge?book="), "{}", r.body);
    let m = anna.get(&merge_url).await.body;
    assert!(m.contains("Merge 2 contacts") && m.contains("Keep this one") && m.contains("Lovelace, Ada"), "{m}");
    assert_eq!(m.matches("name=\"phone\"").count(), 1, "the same number once");
    // One alone is not a merge.
    let r = anna.post("/contacts/selected", &[("pick", &format!("{bs}/ada1.vcf")), ("action", "merge")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Merge into ada1, keeping its name, everything else.
    let (e1, e2) = (etag("ada1.vcf").await, etag("ada2.vcf").await);
    let form = [
        ("book", bs.as_str()),
        ("keep", "0"),
        ("n", "ada1.vcf"),
        ("e", &e1),
        ("n", "ada2.vcf"),
        ("e", &e2),
        ("name", "0"),
        ("birthday", "0"),
        ("phone", "0"),
        ("email", "0"),
        ("group", "0"),
        ("note", "Met at the lab"),
    ];
    let r = anna.post("/contacts/merge", &form).await;
    let back = loc(&r);
    assert!(back.starts_with(&format!("/contacts/{bs}/ada1.vcf?merged=")), "{}", r.body);
    let page = anna.get(&back).await.body;
    assert!(page.contains("Merged.") && page.contains("/contacts/undo/"));
    let merged = objects::get(&db.pool, anna_id, b, "ada1.vcf").await.unwrap().body;
    for want in [
        "UID:ada1",
        "FN:Ada Lovelace",
        "TEL;TYPE=CELL:+44 7700 900001",
        "X-SOCIALPROFILE:ada-social",
        "CATEGORIES:Family",
        "EMAIL;TYPE=HOME:ada@example.org",
        "BDAY:1815-12-10",
        "NOTE:Met at the lab",
        "URL:https://ada.example",
        "X-ABLabel:Blog",
    ] {
        assert!(merged.contains(want), "{want} in {merged}");
    }
    assert_eq!(merged.matches("TEL").count(), 1);
    assert!(objects::get(&db.pool, anna_id, b, "ada2.vcf").await.is_err(), "the other card is gone");
    // The same form again: the cards changed, so nothing happens.
    let r = anna.post("/contacts/merge", &form).await;
    assert!(loc(&r).contains("stale=1"));
    // Undo puts both back as they were.
    let change = back.split("merged=").nth(1).unwrap();
    let r = anna.post(&format!("/contacts/undo/{change}"), &[]).await;
    assert_eq!(loc(&r), "/contacts?undone=1");
    assert_eq!(objects::get(&db.pool, anna_id, b, "ada1.vcf").await.unwrap().body, ADA);
    assert_eq!(objects::get(&db.pool, anna_id, b, "ada2.vcf").await.unwrap().body, ADA2);

    // Bulk delete: a check page first, the box must be ticked, one Undo.
    let picks = [("pick", format!("{bs}/bare.vcf")), ("pick", format!("{bs}/bea2.vcf")), ("action", "delete".into())];
    let picks: Vec<(&str, &str)> = picks.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let r = anna.post("/contacts/selected", &picks).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Delete 2 contacts?") && r.body.contains("Old Import") && r.body.contains("required"));
    assert!(objects::get(&db.pool, anna_id, b, "bare.vcf").await.is_ok(), "nothing deleted yet");
    let (eb, e2) = (etag("bare.vcf").await, etag("bea2.vcf").await);
    let mut del = vec![
        ("c", bs.as_str()),
        ("n", "bare.vcf"),
        ("e", eb.as_str()),
        ("c", bs.as_str()),
        ("n", "bea2.vcf"),
        ("e", e2.as_str()),
    ];
    assert_eq!(anna.post("/contacts/delete-many", &del).await.status, StatusCode::BAD_REQUEST, "not confirmed");
    // A stale ETag refuses all of it.
    let mut stale = del.clone();
    stale[2] = ("e", "\"1\"");
    stale.push(("confirm", "yes"));
    let r = anna.post("/contacts/delete-many", &stale).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(objects::get(&db.pool, anna_id, b, "bea2.vcf").await.is_ok(), "all or nothing");
    del.push(("confirm", "yes"));
    let r = anna.post("/contacts/delete-many", &del).await;
    let gone = loc(&r);
    assert!(gone.starts_with("/contacts?deleted=") && gone.ends_with("&n=2"), "{}", r.body);
    let l = anna.get(&gone).await.body;
    assert!(l.contains("2 contacts deleted.") && !l.contains("Old Import"));
    let change = gone.split("deleted=").nth(1).unwrap().split('&').next().unwrap();
    anna.post(&format!("/contacts/undo/{change}"), &[]).await;
    assert_eq!(objects::get(&db.pool, anna_id, b, "bare.vcf").await.unwrap().body, BARE);
    assert_eq!(objects::get(&db.pool, anna_id, b, "bea2.vcf").await.unwrap().body, BEA2);

    // Someone who can only view the book: no picking, no merge, no deletes.
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;
    collections::set_grant(&db.pool, anna_id, b, bob_id, Some(Access::Read)).await.unwrap();
    let l = bob.get("/contacts?select=1").await.body;
    assert!(!l.contains("name=\"pick\"") && !l.contains("/contacts/duplicates\""), "{l}");
    assert!(!bob.get("/contacts/duplicates").await.body.contains("dup-group"));
    assert_eq!(bob.get(&merge_url).await.status, StatusCode::FORBIDDEN);
    let eb = etag("bare.vcf").await;
    let r = bob.post("/contacts/delete-many", &[("c", &bs), ("n", "bare.vcf"), ("e", &eb), ("confirm", "yes")]).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(
        bob.post("/contacts/duplicates/apart", &[("book", &bs), ("n", "a"), ("n", "b")]).await.status,
        StatusCode::FORBIDDEN
    );
    db.drop().await;
}
