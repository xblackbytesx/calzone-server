//! The contacts pages against a real Postgres (phase 2 step 4): add, find,
//! view, edit without losing anything, stale edits, delete and undo,
//! photos, read-only address books, Dutch.

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
    lang: &'static str,
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
        (Who { app: app.clone(), cookie: format!("calzone_session={token}"), lang: "en" }, id)
    }

    async fn get(&self, path: &str) -> Reply {
        send(&self.app, "GET", path, None, &[("cookie", &self.cookie), ("accept-language", self.lang)], "").await
    }

    async fn post(&self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let body = fields.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&");
        let h = [
            ("cookie", self.cookie.as_str()),
            ("content-type", "application/x-www-form-urlencoded"),
            ("accept-language", self.lang),
        ];
        send(&self.app, "POST", path, None, &h, &body).await
    }
}

fn loc(r: &Reply) -> String {
    r.header("location").unwrap_or_default()
}

async fn book(db: &Db, owner: Uuid, slug: &str) -> Uuid {
    let new = NewCollection {
        kind: CollectionKind::AddressBook,
        slug,
        display_name: "Contacts",
        description: "",
        color: None,
        components: &[],
    };
    collections::create(&db.pool, owner, new).await.unwrap().id
}

/// The form fields for a contact with one phone.
fn fields<'a>(book: &'a str, given: &'a str, phone: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("book", book),
        ("etag", ""),
        ("birthday_was", ""),
        ("given", given),
        ("family", "Lovelace"),
        ("org", ""),
        ("phone_kind", "mobile"),
        ("phone", phone),
        ("phone_kind", "home"),
        ("phone", ""),
        ("email_kind", "home"),
        ("email", "ada@example.org"),
        ("addr_kind", "home"),
        ("addr_street", ""),
        ("addr_postcode", ""),
        ("addr_city", ""),
        ("addr_region", ""),
        ("addr_country", ""),
        ("birthday", "1815-12-10"),
        ("note", "Poet"),
    ]
}

#[tokio::test]
async fn add_find_view_edit_delete_undo() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let b = book(&db, anna_id, "contacts").await;
    let bs = b.to_string();
    let list = anna.get("/contacts").await;
    assert_eq!(list.status, StatusCode::OK);
    assert!(list.body.contains("No contacts yet.") && list.body.contains("href=\"/contacts/new\""));

    // Add.
    let r = anna.post("/contacts/new", &fields(&bs, "Ada", "+44 7700 900001")).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER, "{}", r.body);
    let added = loc(&r);
    assert!(added.contains("?added="));
    let page_path = added.split('?').next().unwrap().to_string();
    let page = anna.get(&added).await;
    assert!(page.body.contains("<h1>Ada Lovelace</h1>") && page.body.contains("href=\"tel:+447700900001\""));
    assert!(page.body.contains("href=\"mailto:ada@example.org\"") && page.body.contains("10 December 1815"));
    assert!(page.body.contains("Added.") && page.body.contains("/contacts/undo/"));
    let name = page_path.rsplit('/').next().unwrap().to_string();
    let stored = objects::get(&db.pool, anna_id, b, &name).await.unwrap().body;
    assert!(stored.contains("VERSION:3.0") && stored.contains("TEL;TYPE=CELL:+44 7700 900001"));
    assert_eq!(stored.matches("TEL").count(), 1, "the empty phone row is dropped");
    assert!(stored.contains("EMAIL;TYPE=HOME:ada@example.org") && stored.contains("BDAY:1815-12-10"));

    // Find: by name, by phone digits; not by something else.
    for q in ["ada", "lovelace", "7700900", "example.org"] {
        assert!(anna.get(&format!("/contacts?q={q}")).await.body.contains("Ada Lovelace"), "{q}");
    }
    assert!(anna.get("/contacts?q=bob").await.body.contains("No one matches."));

    // Edit keeps what the form does not show: a card from a phone.
    let from_phone = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:bea\r\nFN:Bea Smith\r\nN:Smith;Bea;;;\r\nTEL;VALUE=uri;TYPE=\"voice,cell\":tel:+31-6-11\r\n\
BDAY:--0315\r\nX-PHONETIC-FIRST-NAME:Bee\r\nPHOTO:data:image/png;base64,iVBORw0KGgo=\r\nEND:VCARD\r\n";
    objects::put(&db.pool, anna_id, b, "bea.vcf", from_phone, Precondition::None).await.unwrap();
    let form = anna.get(&format!("/contacts/{bs}/bea.vcf/edit")).await;
    assert!(
        form.body.contains("value=\"+31-6-11\"") && form.body.contains("name=\"birthday\" value=\"\""),
        "yearless birthday not in the date field"
    );
    let etag = objects::get(&db.pool, anna_id, b, "bea.vcf").await.unwrap().meta.etag();
    let mut f = fields("", "Bea", "+31-6-11");
    f[1] = ("etag", &etag);
    f[4] = ("family", "Smith");
    f[11] = ("email", "");
    f[18] = ("birthday", "");
    f[19] = ("note", "Neighbour");
    let r = anna.post(&format!("/contacts/{bs}/bea.vcf"), &f).await;
    assert!(loc(&r).contains("?saved="), "{}", r.body);
    let after = objects::get(&db.pool, anna_id, b, "bea.vcf").await.unwrap().body;
    for keep in [
        "TEL;VALUE=uri;TYPE=\"voice,cell\":tel:+31-6-11",
        "BDAY:--0315",
        "X-PHONETIC-FIRST-NAME:Bee",
        "PHOTO:data:image/png;base64,iVBORw0KGgo=",
        "FN:Bea Smith",
    ] {
        assert!(after.contains(keep), "{keep} kept");
    }
    assert!(after.contains("NOTE:Neighbour") && after.contains("REV:"));
    // The photo is served as an image.
    let ph = anna.get(&format!("/contacts/photo/{bs}/bea.vcf")).await;
    assert_eq!((ph.status, ph.header("content-type").as_deref()), (StatusCode::OK, Some("image/png")));
    assert!(anna.get("/contacts").await.body.contains(&format!("src=\"/contacts/photo/{bs}/bea.vcf\"")));
    assert_eq!(anna.get(&format!("/contacts/photo/{bs}/{name}")).await.status, StatusCode::NOT_FOUND, "no photo");

    // A stale form is not saved over a newer edit.
    let r = anna.post(&format!("/contacts/{bs}/bea.vcf"), &f).await;
    assert!(loc(&r).ends_with("?stale=1"));
    assert!(anna.get(&loc(&r)).await.body.contains("changed somewhere else"));

    // Delete, then undo.
    let etag = objects::get(&db.pool, anna_id, b, &name).await.unwrap().meta.etag();
    let r = anna.post(&format!("{page_path}/delete"), &[("etag", &etag)]).await;
    let deleted = loc(&r);
    assert!(deleted.starts_with("/contacts?deleted="));
    let list = anna.get(&deleted).await.body;
    assert!(list.contains("Deleted.") && !list.contains("Ada Lovelace"));
    let change = deleted.split('=').nth(1).unwrap();
    let r = anna.post(&format!("/contacts/undo/{change}"), &[]).await;
    assert_eq!(loc(&r), "/contacts?undone=1");
    let list = anna.get("/contacts?undone=1").await.body;
    assert!(list.contains("Undone.") && list.contains("Ada Lovelace"));

    // Nobody else's: not found.
    let (dave, _) = Who::new(&db, &app, "dave").await;
    assert_eq!(dave.get(&page_path).await.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

#[tokio::test]
async fn shared_read_only_and_dutch() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (mut bob, bob_id) = Who::new(&db, &app, "bob").await;
    let b = book(&db, anna_id, "contacts").await;
    collections::set_grant(&db.pool, anna_id, b, bob_id, Some(Access::Read)).await.unwrap();
    let r = anna.post("/contacts/new", &fields(&b.to_string(), "Ada", "+44 1")).await;
    let path = loc(&r).split('?').next().unwrap().to_string();
    bob.lang = "nl-NL,nl;q=0.9";
    let page = bob.get(&path).await.body;
    assert!(
        page.contains("Je kunt dit adresboek bekijken")
            && page.contains(">Bellen<")
            && page.contains("10 december 1815")
    );
    assert!(!page.contains("/edit\"") && !page.contains("/delete\""));
    assert_eq!(bob.get(&format!("{path}/edit")).await.status, StatusCode::FORBIDDEN);
    let list = bob.get("/contacts").await.body;
    assert!(list.contains("Contacten") && !list.contains("href=\"/contacts/new\""), "nothing he can add to");
    assert_eq!(bob.get("/contacts/new").await.status, StatusCode::BAD_REQUEST);
    // A contact needs a name or an organisation.
    let bs = b.to_string();
    let mut f = fields(&bs, "", "+44 2");
    f[4] = ("family", "");
    let r = anna.post("/contacts/new", &f).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    assert!(r.body.contains("Give a name or an organisation."));
    db.drop().await;
}
