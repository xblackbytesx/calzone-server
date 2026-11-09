//! The sync API against a real Postgres (phase 4 step 2, decision 47):
//! signing a phone in with a code, the collection list with the person's
//! own view, change batches, uploads merged with the core, two phones
//! editing offline with a web edit in between, and push messages that wake
//! the right phones and nobody else.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use calzone_core::sync::Notice;
use calzone_server::domain::prefs::{self, CalendarPrefs};
use calzone_server::domain::{
    Access, CollectionKind, NewCollection, Precondition, accounts, collections, objects, principals,
};
use calzone_server::{AppState, push, router};
use common::{Db, send};
use serde_json::{Value, json};
use uuid::Uuid;

/// A phone signed in with a device token.
struct Phone {
    app: Router,
    token: String,
}

impl Phone {
    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let auth = format!("Bearer {}", self.token);
        let h = [("authorization", auth.as_str()), ("content-type", "application/json")];
        let r = send(&self.app, method, path, None, &h, &body.map(|b| b.to_string()).unwrap_or_default()).await;
        (r.status, serde_json::from_str(&r.body).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> Value {
        let (s, v) = self.call("GET", path, None).await;
        assert_eq!(s, StatusCode::OK, "{path}: {v}");
        v
    }

    /// Upload edits to a collection; the answer.
    async fn upload(&self, cal: Uuid, changes: Value) -> Value {
        let (s, v) = self
            .call("POST", &format!("/sync/v1/collections/{cal}/changes"), Some(json!({ "changes": changes })))
            .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    }

    /// Every change since `since`, following `more`; the last batch's state.
    async fn pull(&self, cal: Uuid, since: Option<i64>, limit: usize) -> (Vec<Value>, i64) {
        let mut all = Vec::new();
        let mut since = since;
        loop {
            let q = match since {
                Some(s) => format!("?since={s}&limit={limit}"),
                None => format!("?limit={limit}"),
            };
            let b = self.get(&format!("/sync/v1/collections/{cal}/changes{q}")).await;
            all.extend(b["changes"].as_array().unwrap().iter().cloned());
            let state = b["state"].as_i64().unwrap();
            if !b["more"].as_bool().unwrap() {
                return (all, state);
            }
            since = Some(state);
        }
    }
}

/// A signed-in browser session's cookie header value.
async fn cookie(db: &Db, user: Uuid) -> String {
    format!("calzone_session={}", accounts::create_session(&db.pool, user, true).await.unwrap())
}

/// Show a sign-in code on the Devices page as `user`; the code.
async fn show_code(app: &Router, cookie: &str) -> String {
    let r = send(app, "POST", "/devices/link", None, &[("cookie", cookie)], "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("<svg"), "a QR code");
    let at = r.body.find("czl_").expect("the code in the page");
    r.body[at..at + 47].to_string()
}

async fn claim(app: &Router, code: &str, device: &str) -> (StatusCode, Value) {
    let r = send(
        app,
        "POST",
        "/sync/v1/link",
        None,
        &[("content-type", "application/json")],
        &json!({ "code": code, "device": device }).to_string(),
    )
    .await;
    (r.status, serde_json::from_str(&r.body).unwrap_or(Value::Null))
}

async fn sign_in(db: &Db, app: &Router, user: Uuid, device: &str) -> Phone {
    let code = show_code(app, &cookie(db, user).await).await;
    let (s, v) = claim(app, &code, device).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    Phone { app: app.clone(), token: v["token"].as_str().unwrap().to_string() }
}

async fn calendar(db: &Db, owner: Uuid, slug: &str) -> Uuid {
    let new = NewCollection {
        kind: CollectionKind::Calendar,
        slug,
        display_name: "",
        description: "",
        color: Some("#112233"),
        components: &[],
    };
    collections::create(&db.pool, owner, new).await.unwrap().id
}

fn event(uid: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//test//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
         DTSTART:20260310T090000Z\r\nDTEND:20260310T100000Z\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn alarm(minutes: u32) -> String {
    format!("BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:x\r\nTRIGGER:-PT{minutes}M\r\nEND:VALARM\r\n")
}

fn body_of<'a>(changes: &'a [Value], name: &str) -> Option<&'a str> {
    changes.iter().rev().find(|c| c["name"] == name).and_then(|c| c["body"].as_str())
}

#[tokio::test]
async fn a_phone_signs_in_with_a_code() {
    let Some(db) = Db::new().await else { return };
    let anna = principals::create_user(&db.pool, "anna", "Anna").await.unwrap();
    // The door off: phones still sign in and sync (decision 3).
    let app = router(AppState { dav: false, ..db.state() });
    let web = cookie(&db, anna).await;

    let page = send(&app, "GET", "/devices", None, &[("cookie", &web)], "").await.body;
    assert!(page.contains("Phones with the CalZone app") && !page.contains("Other apps (DAVx5"));
    let code = show_code(&app, &web).await;
    let landing = send(&app, "GET", &format!("/link/{code}"), None, &[("accept-language", "nl")], "").await;
    assert_eq!(landing.status, StatusCode::OK);
    assert!(
        landing.body.contains("CalZone-app"),
        "a camera app opening the code lands on a page in the reader's language"
    );

    let (s, v) = claim(&app, &code, "  Pixel\u{7} 9 ").await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let token = v["token"].as_str().unwrap().to_string();
    assert!(token.starts_with("czd_") && token.len() == 47);
    assert_eq!(v["account"]["name"], "anna");
    assert_eq!(v["account"]["display_name"], "Anna");
    assert_eq!(v["account"]["device_label"], "Pixel 9");
    assert_eq!(v["account"]["push"], false);
    let (s, v) = claim(&app, &code, "Second").await;
    assert_eq!((s, v["code"].as_str()), (StatusCode::FORBIDDEN, Some("link_used")), "a code works once");
    assert_eq!(claim(&app, "czl_nonsense", "x").await.0, StatusCode::FORBIDDEN);

    let phone = Phone { app: app.clone(), token: token.clone() };
    assert_eq!(phone.get("/sync/v1/me").await["device_label"], "Pixel 9");
    let page = send(&app, "GET", "/devices", None, &[("cookie", &web)], "").await.body;
    assert!(page.contains("Pixel 9"));

    // Expired codes, bad tokens, disabled people.
    let late = show_code(&app, &web).await;
    sqlx::query("UPDATE device_links SET expires_at = now() - interval '1 second'").execute(&db.pool).await.unwrap();
    assert_eq!(claim(&app, &late, "Late").await.0, StatusCode::FORBIDDEN);
    let stranger = Phone { app: app.clone(), token: format!("czd_{}", "A".repeat(43)) };
    let (s, v) = stranger.call("GET", "/sync/v1/me", None).await;
    assert_eq!((s, v["code"].as_str()), (StatusCode::UNAUTHORIZED, Some("unauthorized")));
    principals::set_disabled(&db.pool, anna, true).await.unwrap();
    assert_eq!(phone.call("GET", "/sync/v1/collections", None).await.0, StatusCode::UNAUTHORIZED);
    principals::set_disabled(&db.pool, anna, false).await.unwrap();
    assert_eq!(phone.call("GET", "/sync/v1/collections", None).await.0, StatusCode::OK);

    // Removed on the Devices page: signed out at once. Another person
    // cannot remove it.
    let id = calzone_server::domain::devices::list(&db.pool, anna).await.unwrap()[0].id;
    let bob = principals::create_user(&db.pool, "bob", "").await.unwrap();
    let bob_web = cookie(&db, bob).await;
    let r = send(&app, "POST", &format!("/devices/phones/{id}/remove"), None, &[("cookie", &bob_web)], "").await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let r = send(&app, "POST", &format!("/devices/phones/{id}/remove"), None, &[("cookie", &web)], "").await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    assert_eq!(phone.call("GET", "/sync/v1/me", None).await.0, StatusCode::UNAUTHORIZED);

    // A phone signs itself out.
    let other = sign_in(&db, &app, anna, "Tablet").await;
    assert_eq!(other.call("DELETE", "/sync/v1/me", None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(other.call("GET", "/sync/v1/me", None).await.0, StatusCode::UNAUTHORIZED);
    db.drop().await;
}

#[tokio::test]
async fn collections_and_changes() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let anna = principals::create_user(&db.pool, "anna", "Anna").await.unwrap();
    let bob = principals::create_user(&db.pool, "bob", "").await.unwrap();
    let family = calendar(&db, anna, "family").await;
    let work = calendar(&db, anna, "work").await;
    collections::set_grant(&db.pool, anna, family, bob, Some(Access::Write)).await.unwrap();
    let book = collections::create(
        &db.pool,
        anna,
        NewCollection {
            kind: CollectionKind::AddressBook,
            slug: "contacts",
            display_name: "Contacts",
            description: "",
            color: None,
            components: &[],
        },
    )
    .await
    .unwrap()
    .id;
    let p = CalendarPrefs { color: Some("#aa0000".into()), hidden: true, ..Default::default() };
    prefs::set_calendar_prefs(&db.pool, bob, family, &p).await.unwrap();

    let a = sign_in(&db, &app, anna, "Anna's phone").await;
    let b = sign_in(&db, &app, bob, "Bob's phone").await;
    let list = b.get("/sync/v1/collections").await;
    let cols = list["collections"].as_array().unwrap();
    assert_eq!(cols.len(), 1, "bob sees only what was shared with him: {list}");
    assert_eq!(cols[0]["id"], family.to_string());
    assert_eq!(cols[0]["owner"], "anna");
    assert_eq!(cols[0]["owner_name"], "Anna");
    assert_eq!(cols[0]["access"], "write");
    assert_eq!(cols[0]["read_only"], false);
    assert_eq!(cols[0]["mine"]["color"], "#aa0000");
    assert_eq!(cols[0]["mine"]["hidden"], true);
    let mine = a.get("/sync/v1/collections").await;
    assert_eq!(mine["collections"].as_array().unwrap().len(), 3);
    assert!(mine["collections"].as_array().unwrap().iter().any(|c| c["kind"] == "addressbook"));
    let (s, _) = b.call("GET", &format!("/sync/v1/collections/{work}/changes"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "not shared: not found, never forbidden");
    let (s, _) = b.call("GET", "/sync/v1/collections/not-a-uuid/changes", None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = a.call("GET", &format!("/sync/v1/collections/{book}/changes"), None).await;
    assert_eq!(s, StatusCode::OK);

    // Five events, anna with her own reminder on one.
    for i in 0..5 {
        let extra = if i == 0 { format!("SUMMARY:e{i}\r\n{}", alarm(10)) } else { format!("SUMMARY:e{i}\r\n") };
        objects::put(
            &db.pool,
            anna,
            family,
            &format!("e{i}.ics"),
            &event(&format!("e{i}"), &extra),
            Precondition::None,
        )
        .await
        .unwrap();
    }
    let (full, state) = b.pull(family, None, 2).await;
    assert_eq!(full.len(), 5, "a full sync in batches of two misses nothing");
    assert_eq!(state, 5);
    assert!(!body_of(&full, "e0.ics").unwrap().contains("VALARM"), "anna's reminder never reaches bob");
    let (anna_full, _) = a.pull(family, None, 200).await;
    assert!(body_of(&anna_full, "e0.ics").unwrap().contains("TRIGGER:-PT10M"), "and always reaches anna");

    // An edit, a delete, a delete-then-recreate: each once, newest state.
    objects::put(&db.pool, anna, family, "e1.ics", &event("e1", "SUMMARY:e1 moved\r\n"), Precondition::None)
        .await
        .unwrap();
    objects::delete(&db.pool, anna, family, "e2.ics", Precondition::None).await.unwrap();
    objects::delete(&db.pool, anna, family, "e3.ics", Precondition::None).await.unwrap();
    objects::put(&db.pool, anna, family, "e3.ics", &event("e3", "SUMMARY:back\r\n"), Precondition::None).await.unwrap();
    let (delta, state) = b.pull(family, Some(state), 200).await;
    let names: Vec<&str> = delta.iter().map(|c| c["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["e1.ics", "e2.ics", "e3.ics"]);
    assert!(delta[1].get("body").is_none(), "a tombstone has no body");
    assert!(body_of(&delta, "e3.ics").unwrap().contains("SUMMARY:back"));
    assert_eq!(state, 9);
    // A full sync leaves tombstones out of its first batch; later batches
    // are changes since, so a tombstone for something the phone never had
    // can come along (and changes nothing).
    let first = b.get(&format!("/sync/v1/collections/{family}/changes")).await;
    assert_eq!(first["changes"].as_array().unwrap().len(), 4);
    assert!(first["changes"].as_array().unwrap().iter().all(|c| c["body"].is_string()));
    let (full, _) = b.pull(family, None, 1).await;
    assert_eq!(full.iter().filter(|c| c["body"].is_string()).count(), 4);
    // Nothing new: an empty batch at the same state.
    let empty = b.get(&format!("/sync/v1/collections/{family}/changes?since={state}")).await;
    assert_eq!(empty["changes"].as_array().unwrap().len(), 0);
    assert_eq!(empty["state"], state);
    let (s, v) = b.call("GET", &format!("/sync/v1/collections/{family}/changes?since=99"), None).await;
    assert_eq!((s, v["code"].as_str()), (StatusCode::CONFLICT, Some("bad_state")), "a state never issued: resync");
    db.drop().await;
}

#[tokio::test]
async fn two_phones_offline_and_a_web_edit() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let anna = principals::create_user(&db.pool, "anna", "").await.unwrap();
    let bob = principals::create_user(&db.pool, "bob", "").await.unwrap();
    let carol = principals::create_user(&db.pool, "carol", "").await.unwrap();
    let family = calendar(&db, anna, "family").await;
    collections::set_grant(&db.pool, anna, family, bob, Some(Access::Write)).await.unwrap();
    collections::set_grant(&db.pool, anna, family, carol, Some(Access::Read)).await.unwrap();
    let base = event("dentist", "SUMMARY:Dentist\r\nLOCATION:Old street\r\n");
    objects::put(&db.pool, anna, family, "dentist.ics", &base, Precondition::None).await.unwrap();
    let a = sign_in(&db, &app, anna, "A").await;
    let b = sign_in(&db, &app, bob, "B").await;
    let c = sign_in(&db, &app, carol, "C").await;
    let (pulled, _) = a.pull(family, None, 200).await;
    let a_base = body_of(&pulled, "dentist.ics").unwrap().to_string();
    let (pulled, b_state) = b.pull(family, None, 200).await;
    let b_base = body_of(&pulled, "dentist.ics").unwrap().to_string();

    // Offline: A renames it; B renames it differently and adds a note; the
    // web moves it to another street in between.
    let a_edit = a_base.replace("SUMMARY:Dentist", "SUMMARY:Dentist (Mila)");
    let b_edit = b_base
        .replace("SUMMARY:Dentist", "SUMMARY:Tandarts")
        .replace("LOCATION:Old street\r\n", "LOCATION:Old street\r\nDESCRIPTION:Bring the card\r\n");
    let web =
        objects::get(&db.pool, anna, family, "dentist.ics").await.unwrap().body.replace("Old street", "New street");
    objects::put(&db.pool, anna, family, "dentist.ics", &web, Precondition::None).await.unwrap();

    let ans = a.upload(family, json!([{ "name": "dentist.ics", "base": a_base, "body": a_edit }])).await;
    let merged = ans["pushed"][0]["body"].as_str().unwrap();
    assert!(merged.contains("SUMMARY:Dentist (Mila)") && merged.contains("LOCATION:New street"), "{merged}");
    assert!(ans["pushed"][0].get("conflicts").is_none());

    let ans = b.upload(family, json!([{ "name": "dentist.ics", "base": b_base, "body": b_edit }])).await;
    let merged = ans["pushed"][0]["body"].as_str().unwrap().to_string();
    assert!(merged.contains("SUMMARY:Dentist (Mila)"), "the same field: the server's value stays");
    assert!(merged.contains("DESCRIPTION:Bring the card") && merged.contains("LOCATION:New street"), "{merged}");
    let lost = &ans["pushed"][0]["conflicts"][0]["lost"];
    assert_eq!(lost["kind"], "field");
    assert_eq!(lost["field"], "SUMMARY");
    assert_eq!(lost["lost"][0], "SUMMARY:Tandarts", "the loser is reported");

    // Both phones end on the same copy; nothing duplicated.
    let (delta, _) = b.pull(family, Some(b_state), 200).await;
    assert_eq!(body_of(&delta, "dentist.ics").unwrap(), merged);
    let (all, _) = a.pull(family, None, 200).await;
    assert_eq!(all.len(), 1);
    assert_eq!(body_of(&all, "dentist.ics").unwrap(), merged);

    // Sending the merged copy back changes nothing: no new revision.
    let rev = ans["pushed"][0]["revision"].as_i64().unwrap();
    let again = b.upload(family, json!([{ "name": "dentist.ics", "base": merged, "body": merged }])).await;
    assert_eq!(again["pushed"][0]["revision"].as_i64().unwrap(), rev);
    assert_eq!(objects::get(&db.pool, anna, family, "dentist.ics").await.unwrap().meta.revision, rev);

    // A phone deletes what the web changed: the server's copy stays, reported.
    let stale = merged.clone();
    let web = merged.replace("New street", "Third street");
    objects::put(&db.pool, anna, family, "dentist.ics", &web, Precondition::None).await.unwrap();
    let ans = b.upload(family, json!([{ "name": "dentist.ics", "base": stale }])).await;
    assert!(ans["pushed"][0]["body"].as_str().unwrap().contains("Third street"));
    assert_eq!(ans["pushed"][0]["conflicts"][0]["lost"]["kind"], "delete");

    // New on the phone, an invalid one and a UID that is taken: the good
    // ones go through, the others are reported.
    let ans = a
        .upload(
            family,
            json!([
                { "name": "party.ics", "body": event("party", "SUMMARY:Party\r\n") },
                { "name": "broken.ics", "body": "BEGIN:VCARD\r\nFN:x\r\nEND:VCARD\r\n" },
                { "name": "copy.ics", "body": event("dentist", "SUMMARY:Copy\r\n") },
            ]),
        )
        .await;
    assert_eq!(ans["pushed"].as_array().unwrap().len(), 1);
    assert_eq!(ans["pushed"][0]["name"], "party.ics");
    let reasons: Vec<&str> =
        ans["rejected"].as_array().unwrap().iter().map(|r| r["reason"].as_str().unwrap()).collect();
    assert_eq!(reasons, ["invalid", "uid_taken"]);
    let (s, _) = a
        .call(
            "POST",
            &format!("/sync/v1/collections/{family}/changes"),
            Some(json!({ "changes": [{ "name": "x.ics" }, { "name": "x.ics" }] })),
        )
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "one object at most once per upload");

    // A delete from a phone that may write.
    let party = objects::get(&db.pool, anna, family, "party.ics").await.unwrap().body;
    let ans = b.upload(family, json!([{ "name": "party.ics", "base": party }])).await;
    assert!(ans["pushed"][0].get("body").is_none());
    assert!(objects::get(&db.pool, anna, family, "party.ics").await.is_err());

    // Read access: edits are refused, the person's own reminder goes through
    // and stays theirs (decision 8).
    let (pulled, _) = c.pull(family, None, 200).await;
    let c_base = body_of(&pulled, "dentist.ics").unwrap().to_string();
    let ans = c
        .upload(family, json!([{ "name": "dentist.ics", "base": c_base, "body": c_base.replace("Third", "Fourth") }]))
        .await;
    assert_eq!(ans["rejected"][0]["reason"], "read_only");
    let with_alarm = c_base.replace("END:VEVENT", &format!("{}END:VEVENT", alarm(30)));
    let ans = c.upload(family, json!([{ "name": "dentist.ics", "base": c_base, "body": with_alarm }])).await;
    assert!(ans["pushed"][0]["body"].as_str().unwrap().contains("TRIGGER:-PT30M"), "{ans}");
    assert!(!objects::get(&db.pool, anna, family, "dentist.ics").await.unwrap().body.contains("VALARM"));
    let ans = c.upload(family, json!([{ "name": "new.ics", "body": event("new", "") }])).await;
    assert_eq!(ans["rejected"][0]["reason"], "read_only");
    db.drop().await;
}

/// What the stand-in push server received: (path, body) per message.
type Inbox = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

/// A stand-in push server: records what it receives; `/gone` answers 410.
async fn push_server() -> (String, Inbox) {
    let got: Inbox = Arc::default();
    let rec = got.clone();
    let app = Router::new().fallback(move |req: axum::extract::Request| {
        let rec = rec.clone();
        async move {
            let path = req.uri().path().to_string();
            let encoding =
                req.headers().get("content-encoding").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
            assert_eq!(encoding, "aes128gcm", "pushes are always encrypted");
            let body = axum::body::to_bytes(req.into_body(), 1 << 20).await.unwrap().to_vec();
            let gone = path == "/gone";
            rec.lock().unwrap().push((path, body));
            if gone { StatusCode::GONE } else { StatusCode::CREATED }
        }
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("127.0.0.1:{}", addr.port()), got)
}

/// A phone's WebPush keys: the secret, and the public key and auth secret
/// it registers (base64url).
fn phone_keys() -> (p256::SecretKey, String, [u8; 16]) {
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let secret = loop {
        let mut b = [0u8; 32];
        getrandom::fill(&mut b).unwrap();
        if let Ok(k) = p256::SecretKey::from_slice(&b) {
            break k;
        }
    };
    let public = URL_SAFE_NO_PAD.encode(secret.public_key().to_encoded_point(false).as_bytes());
    let mut auth = [0u8; 16];
    getrandom::fill(&mut auth).unwrap();
    (secret, public, auth)
}

/// RFC 8291 from the phone's side: what the push says.
fn decrypt(secret: &p256::SecretKey, auth: &[u8; 16], body: &[u8]) -> Notice {
    use aes_gcm::aead::{Aead, KeyInit};
    use p256::elliptic_curve::sec1::ToEncodedPoint;
    let salt = &body[..16];
    let id_len = body[20] as usize;
    let server = p256::PublicKey::from_sec1_bytes(&body[21..21 + id_len]).unwrap();
    let sealed = &body[21 + id_len..];
    let shared = p256::ecdh::diffie_hellman(secret.to_nonzero_scalar(), server.as_affine());
    let mut info = b"WebPush: info\0".to_vec();
    info.extend_from_slice(secret.public_key().to_encoded_point(false).as_bytes());
    info.extend_from_slice(server.to_encoded_point(false).as_bytes());
    let mut ikm = [0u8; 32];
    hkdf::Hkdf::<sha2::Sha256>::new(Some(auth), shared.raw_secret_bytes()).expand(&info, &mut ikm).unwrap();
    let prk = hkdf::Hkdf::<sha2::Sha256>::new(Some(salt), &ikm);
    let (mut cek, mut nonce) = ([0u8; 16], [0u8; 12]);
    prk.expand(b"Content-Encoding: aes128gcm\0", &mut cek).unwrap();
    prk.expand(b"Content-Encoding: nonce\0", &mut nonce).unwrap();
    let plain =
        aes_gcm::Aes128Gcm::new_from_slice(&cek).unwrap().decrypt(aes_gcm::Nonce::from_slice(&nonce), sealed).unwrap();
    let end = plain.iter().rposition(|&b| b == 2).unwrap();
    serde_json::from_slice(&plain[..end]).unwrap()
}

/// Wait until the stand-in has `n` messages (pushes are gathered for two
/// seconds first).
async fn wait_for(got: &Inbox, n: usize) -> Vec<(String, Vec<u8>)> {
    for _ in 0..100 {
        if got.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    std::mem::take(&mut *got.lock().unwrap())
}

#[tokio::test]
async fn push_wakes_the_phones_that_can_see_the_change() {
    let Some(db) = Db::new().await else { return };
    let (host, got) = push_server().await;
    let settings = Arc::new(push::Settings { allow_http: true, ..push::Settings::new(vec![host.clone()]) });
    let app = router(AppState { push: settings.clone(), ..db.state() });
    let _task = push::start(db.pool.clone(), settings).await.unwrap();

    let anna = principals::create_user(&db.pool, "anna", "").await.unwrap();
    let bob = principals::create_user(&db.pool, "bob", "").await.unwrap();
    let carol = principals::create_user(&db.pool, "carol", "").await.unwrap();
    let family = calendar(&db, anna, "family").await;
    let a = sign_in(&db, &app, anna, "A").await;
    let b = sign_in(&db, &app, bob, "B").await;
    let c = sign_in(&db, &app, carol, "C").await;

    // Registration: declared hosts only, real keys only.
    let (a_secret, a_key, a_auth) = phone_keys();
    let (b_secret, b_key, b_auth) = phone_keys();
    let (_, c_key, c_auth) = phone_keys();
    let reg = |key: &str, auth: &[u8; 16], url: String| json!({ "endpoint": url, "key": key, "auth": URL_SAFE_NO_PAD.encode(auth) });
    let (s, v) = a.call("PUT", "/sync/v1/push", Some(reg(&a_key, &a_auth, "http://elsewhere.example/x".into()))).await;
    assert_eq!((s, v["code"].as_str()), (StatusCode::BAD_REQUEST, Some("bad_endpoint")));
    let (s, _) = a.call("PUT", "/sync/v1/push", Some(reg("AAAA", &a_auth, format!("http://{host}/a")))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    for (p, key, auth, path) in [(&a, &a_key, &a_auth, "a"), (&b, &b_key, &b_auth, "b"), (&c, &c_key, &c_auth, "c")] {
        let (s, v) = p.call("PUT", "/sync/v1/push", Some(reg(key, auth, format!("http://{host}/{path}")))).await;
        assert_eq!(s, StatusCode::NO_CONTENT, "{v}");
    }
    assert_eq!(a.get("/sync/v1/me").await["push"], true);
    let off = Phone { app: db.app(), token: a.token.clone() };
    let (s, v) = off.call("PUT", "/sync/v1/push", Some(reg(&a_key, &a_auth, format!("http://{host}/a")))).await;
    assert_eq!((s, v["code"].as_str()), (StatusCode::BAD_REQUEST, Some("push_off")));

    // Sharing with bob wakes him (a new calendar to show); a write wakes
    // anna and bob, never carol. Many writes in a row are one push each.
    collections::set_grant(&db.pool, anna, family, bob, Some(Access::Write)).await.unwrap();
    for i in 0..5 {
        objects::put(&db.pool, anna, family, &format!("e{i}.ics"), &event(&format!("e{i}"), ""), Precondition::None)
            .await
            .unwrap();
    }
    let msgs = wait_for(&got, 2).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut paths: Vec<&str> = msgs.iter().map(|(p, _)| p.as_str()).collect();
    paths.sort();
    assert_eq!(paths, ["/a", "/b"], "one message each, carol none");
    for (path, body) in &msgs {
        let (secret, auth) = if path == "/a" { (&a_secret, &a_auth) } else { (&b_secret, &b_auth) };
        let n = decrypt(secret, auth, body);
        assert_eq!(n.collections.len(), 1);
        assert_eq!(n.collections[0].id, family.to_string());
        assert_eq!(n.collections[0].state.0, 5, "the state after the last write");
        assert!(!n.all);
    }
    assert!(got.lock().unwrap().is_empty(), "nothing more");

    // Bob's own view of the calendar: only bob hears about it.
    prefs::set_calendar_prefs(
        &db.pool,
        bob,
        family,
        &CalendarPrefs { color: Some("#00aa00".into()), ..Default::default() },
    )
    .await
    .unwrap();
    let msgs = wait_for(&got, 1).await;
    tokio::time::sleep(Duration::from_millis(2500)).await;
    assert_eq!(msgs.len(), 1);
    assert_eq!(msgs[0].0, "/b");
    assert!(got.lock().unwrap().is_empty());

    // A push server that says the endpoint is gone: it is dropped.
    let (s, _) = b.call("PUT", "/sync/v1/push", Some(reg(&b_key, &b_auth, format!("http://{host}/gone")))).await;
    assert_eq!(s, StatusCode::NO_CONTENT);
    objects::put(&db.pool, anna, family, "e9.ics", &event("e9", ""), Precondition::None).await.unwrap();
    wait_for(&got, 2).await;
    for _ in 0..50 {
        if b.get("/sync/v1/me").await["push"] == false {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(b.get("/sync/v1/me").await["push"], false);
    assert_eq!(c.call("DELETE", "/sync/v1/push", None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(c.get("/sync/v1/me").await["push"], false);
    db.drop().await;
}
