//! The phone's engine (`calzone-client`) against the real server over HTTP,
//! on Postgres (phase 4 step 3, decision 48): sign-in, full and
//! incremental sync, two phones and the web API on one calendar with
//! offline edits, refusals, a collection that goes away, a restored
//! server, and a run that fails half way.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::http::StatusCode;
use calzone_client::{ConflictDetail, Request, Response, Store, Transport, engine};
use calzone_server::domain::{
    Access, CollectionKind, NewCollection, Precondition, accounts, collections, objects, principals,
};
use common::{Db, send, serve};
use serde_json::json;
use tokio::runtime::Handle;
use uuid::Uuid;

/// The app's OkHttp, played by reqwest: the server's address in front, the
/// token once signed in. Counts requests; can be told to fail after `n`.
struct Http {
    base: String,
    token: Mutex<Option<String>>,
    http: reqwest::Client,
    rt: Handle,
    sent: AtomicUsize,
    fail_after: Mutex<Option<usize>>,
}

impl Http {
    fn new(base: &str) -> Arc<Http> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Arc::new(Http {
            base: base.to_string(),
            token: Mutex::new(None),
            http: reqwest::Client::new(),
            rt: Handle::current(),
            sent: AtomicUsize::new(0),
            fail_after: Mutex::new(None),
        })
    }
}

impl Transport for Http {
    fn send(&self, r: &Request) -> Result<Response, String> {
        let n = self.sent.fetch_add(1, Ordering::SeqCst);
        if self.fail_after.lock().unwrap().is_some_and(|after| n >= after) {
            return Err("the network went away".into());
        }
        let method = reqwest::Method::from_bytes(r.method.as_str().as_bytes()).unwrap();
        let mut req = self.http.request(method, format!("{}{}", self.base, r.path));
        if let Some(t) = self.token.lock().unwrap().as_ref() {
            req = req.bearer_auth(t);
        }
        if let Some(b) = &r.body {
            req = req.header("content-type", "application/json").body(b.clone());
        }
        self.rt.block_on(async {
            let res = req.send().await.map_err(|e| e.to_string())?;
            let status = res.status().as_u16();
            Ok(Response { status, body: res.text().await.map_err(|e| e.to_string())? })
        })
    }
}

/// A phone: its own store and transport. Engine calls block, as on the
/// phone's sync thread, so they run off the async runtime.
#[derive(Clone)]
struct Phone {
    store: Arc<Store>,
    http: Arc<Http>,
}

impl Phone {
    async fn sync(&self) -> calzone_client::Result<calzone_client::Report> {
        let p = self.clone();
        tokio::task::spawn_blocking(move || engine::sync(&p.store, p.http.as_ref(), 1_000)).await.unwrap()
    }

    async fn synced(&self) -> calzone_client::Report {
        self.sync().await.expect("sync")
    }

    fn body(&self, collection: Uuid, name: &str) -> Option<String> {
        self.store.object(&collection.to_string(), name).unwrap().map(|o| o.body)
    }

    fn edit(&self, collection: Uuid, name: &str, body: Option<&str>) {
        self.store.edit(&collection.to_string(), name, body).unwrap();
    }

    /// Edits waiting for the server.
    fn waiting(&self) -> u32 {
        self.store.collections().unwrap().iter().map(|c| c.pending).sum()
    }

    fn requests(&self) -> usize {
        self.http.sent.load(Ordering::SeqCst)
    }
}

async fn web_cookie(db: &Db, user: Uuid) -> String {
    format!("calzone_session={}", accounts::create_session(&db.pool, user, true).await.unwrap())
}

/// Sign a new phone in the way the app does: a code from the Devices
/// page, its address parsed, the code redeemed through the engine.
async fn phone(db: &Db, app: &axum::Router, base: &str, user: Uuid, label: &str) -> Phone {
    let r = send(app, "POST", "/devices/link", None, &[("cookie", &web_cookie(db, user).await)], "").await;
    let at = r.body.find("czl_").unwrap();
    let url = format!("{base}/link/{}", &r.body[at..at + 47]);
    let (server, code) = calzone_client::link::parse(&url).unwrap();
    assert_eq!(server, base);
    let p = Phone { store: Arc::new(Store::in_memory().unwrap()), http: Http::new(&server) };
    let q = p.clone();
    let label = label.to_string();
    let signed =
        tokio::task::spawn_blocking(move || engine::sign_in(&q.store, q.http.as_ref(), &server, &code, &label))
            .await
            .unwrap()
            .unwrap();
    *p.http.token.lock().unwrap() = Some(signed.token);
    p
}

async fn calendar(db: &Db, owner: Uuid, slug: &str) -> Uuid {
    let new = NewCollection {
        kind: CollectionKind::Calendar,
        slug,
        display_name: "",
        description: "",
        color: None,
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

fn card(uid: &str, name: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:4.0\r\nUID:{uid}\r\nFN:{name}\r\nTEL;TYPE=cell:+31 6 1234 5678\r\nEND:VCARD\r\n")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_phone_syncs_a_calendar_and_contacts() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let base = serve(app.clone()).await;
    let anna = principals::create_user(&db.pool, "anna", "Anna").await.unwrap();
    let family = calendar(&db, anna, "family").await;
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
    for i in 0..30 {
        objects::put(
            &db.pool,
            anna,
            family,
            &format!("e{i}.ics"),
            &event(&format!("e{i}"), &format!("SUMMARY:Event {i}\r\n")),
            Precondition::None,
        )
        .await
        .unwrap();
    }
    for i in 0..1200 {
        objects::put(
            &db.pool,
            anna,
            book,
            &format!("c{i}.vcf"),
            &card(&format!("c{i}"), &format!("Person {i}")),
            Precondition::None,
        )
        .await
        .unwrap();
    }

    let p = phone(&db, &app, &base, anna, "Pixel").await;
    assert_eq!(p.store.account().unwrap().unwrap().device_label, "Pixel");
    let r = p.synced().await;
    assert_eq!((r.collections, r.pulled, r.pushed, r.conflicts), (2, 1230, 0, 0));
    assert_eq!(p.store.objects(&book.to_string()).unwrap().len(), 1200, "1200 cards over three batches");
    assert_eq!(p.body(family, "e7.ics"), Some(objects::get(&db.pool, anna, family, "e7.ics").await.unwrap().body));
    let cards = p.store.objects(&book.to_string()).unwrap();
    assert!(cards.iter().any(|c| c.summary.as_deref() == Some("Person 42") && c.kind == "VCARD"));

    // Nothing new: one request.
    let before = p.requests();
    assert_eq!(p.synced().await, calzone_client::Report { collections: 2, ..Default::default() });
    assert_eq!(p.requests() - before, 1);

    // Edits both ways, deletes both ways.
    objects::put(&db.pool, anna, family, "e1.ics", &event("e1", "SUMMARY:Moved\r\n"), Precondition::None)
        .await
        .unwrap();
    objects::delete(&db.pool, anna, family, "e2.ics", Precondition::None).await.unwrap();
    p.edit(family, "e3.ics", None);
    p.edit(family, "new.ics", Some(&event("new", "SUMMARY:From the phone\r\n")));
    let c5 = p.body(book, "c5.vcf").unwrap().replace("Person 5", "Person Five");
    p.edit(book, "c5.vcf", Some(&c5));
    let seq = p.store.local_changes(0, 10_000).unwrap().last().unwrap().seq;
    let r = p.synced().await;
    assert_eq!((r.pushed, r.rejected, r.conflicts), (3, 0, 0));
    assert!(objects::get(&db.pool, anna, family, "e3.ics").await.is_err());
    assert!(objects::get(&db.pool, anna, family, "new.ics").await.unwrap().body.contains("From the phone"));
    assert!(objects::get(&db.pool, anna, book, "c5.vcf").await.unwrap().body.contains("FN:Person Five"));
    assert!(p.body(family, "e1.ics").unwrap().contains("Moved"));
    assert_eq!(p.body(family, "e2.ics"), None);
    assert_eq!(p.waiting(), 0);
    let log: Vec<(String, bool)> =
        p.store.local_changes(seq, 100).unwrap().into_iter().filter_map(|c| c.name.map(|n| (n, c.deleted))).collect();
    for want in [("e1.ics", false), ("e2.ics", true), ("new.ics", false), ("e3.ics", true)] {
        assert!(log.contains(&(want.0.to_string(), want.1)), "the bridge hears about {want:?}: {log:?}");
    }
    // The phone's own writes come back in the next pull as nothing new.
    let r = p.synced().await;
    assert_eq!(r.pulled, 0);
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn two_phones_and_the_web_on_one_calendar() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let base = serve(app.clone()).await;
    let anna = principals::create_user(&db.pool, "anna", "").await.unwrap();
    let bob = principals::create_user(&db.pool, "bob", "").await.unwrap();
    let family = calendar(&db, anna, "family").await;
    collections::set_grant(&db.pool, anna, family, bob, Some(Access::Write)).await.unwrap();
    objects::put(
        &db.pool,
        anna,
        family,
        "dentist.ics",
        &event("dentist", "SUMMARY:Dentist\r\nLOCATION:Old street\r\n"),
        Precondition::None,
    )
    .await
    .unwrap();
    let a = phone(&db, &app, &base, anna, "A").await;
    let b = phone(&db, &app, &base, bob, "B").await;
    a.synced().await;
    b.synced().await;

    // Offline: A renames it, B renames it too and adds a note; the web
    // (the calendar page's API) moves it to another street meanwhile.
    let a_copy = a.body(family, "dentist.ics").unwrap();
    a.edit(family, "dentist.ics", Some(&a_copy.replace("SUMMARY:Dentist", "SUMMARY:Dentist (Mila)")));
    let b_copy = b.body(family, "dentist.ics").unwrap();
    b.edit(
        family,
        "dentist.ics",
        Some(
            &b_copy
                .replace("SUMMARY:Dentist", "SUMMARY:Tandarts")
                .replace("END:VEVENT", "DESCRIPTION:Bring the card\r\nEND:VEVENT"),
        ),
    );
    let etag = objects::get(&db.pool, anna, family, "dentist.ics").await.unwrap().meta.etag();
    let cookie = web_cookie(&db, anna).await;
    let r = send(
        &app,
        "POST",
        &format!("/api/events/{family}/dentist.ics"),
        None,
        &[("cookie", &cookie), ("content-type", "application/json")],
        &json!({ "etag": etag, "location": "New street" }).to_string(),
    )
    .await;
    assert_eq!(r.status, StatusCode::OK, "{}", r.body);

    let ra = a.synced().await;
    assert_eq!((ra.pushed, ra.conflicts), (1, 0));
    let rb = b.synced().await;
    assert_eq!((rb.pushed, rb.conflicts), (1, 1));
    a.synced().await;

    let server = objects::get(&db.pool, anna, family, "dentist.ics").await.unwrap().body;
    assert!(
        server.contains("SUMMARY:Dentist (Mila)") && server.contains("New street") && server.contains("Bring the card"),
        "{server}"
    );
    assert_eq!(a.body(family, "dentist.ics").unwrap(), server, "both phones end on the server's copy");
    assert_eq!(b.body(family, "dentist.ics").unwrap(), server);
    assert_eq!(a.store.objects(&family.to_string()).unwrap().len(), 1, "nothing duplicated");
    let conflicts = b.store.conflicts().unwrap();
    let ConflictDetail::Merged { conflicts: lost } = &conflicts[0].detail else { panic!("{conflicts:?}") };
    let lost = serde_json::to_value(&lost[0].lost).unwrap();
    assert_eq!(lost["field"], "SUMMARY");
    assert_eq!(lost["lost"][0], "SUMMARY:Tandarts", "the person is told what they lost");

    // Unshared while B has an edit waiting: the calendar goes, the edit
    // becomes a conflict, nothing else breaks.
    b.edit(family, "dentist.ics", Some(&server.replace("New street", "B's street")));
    collections::set_grant(&db.pool, anna, family, bob, None).await.unwrap();
    let r = b.synced().await;
    assert_eq!((r.collections, r.collections_gone, r.conflicts), (0, 1, 1));
    assert!(b.store.collections().unwrap().is_empty());
    assert!(matches!(b.store.conflicts().unwrap().last().unwrap().detail, ConflictDetail::CollectionGone { .. }));
    db.drop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn refusals_failures_and_a_restored_server() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let base = serve(app.clone()).await;
    let anna = principals::create_user(&db.pool, "anna", "").await.unwrap();
    let carol = principals::create_user(&db.pool, "carol", "").await.unwrap();
    let family = calendar(&db, anna, "family").await;
    collections::set_grant(&db.pool, anna, family, carol, Some(Access::Read)).await.unwrap();
    for i in 0..3 {
        objects::put(
            &db.pool,
            anna,
            family,
            &format!("e{i}.ics"),
            &event(&format!("e{i}"), "SUMMARY:x\r\n"),
            Precondition::None,
        )
        .await
        .unwrap();
    }
    let c = phone(&db, &app, &base, carol, "C").await;
    c.synced().await;
    let list = c.store.collections().unwrap();
    assert!(list[0].info.read_only);

    // Read access: an edit is refused and undone here; a reminder goes up
    // and stays carol's.
    let copy = c.body(family, "e0.ics").unwrap();
    c.edit(family, "e0.ics", Some(&copy.replace("SUMMARY:x", "SUMMARY:mine")));
    let alarm = "BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:x\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n";
    let e1 = c.body(family, "e1.ics").unwrap().replace("END:VEVENT", &format!("{alarm}END:VEVENT"));
    c.edit(family, "e1.ics", Some(&e1));
    let r = c.synced().await;
    assert_eq!((r.pushed, r.rejected), (1, 1));
    assert_eq!(c.body(family, "e0.ics").unwrap(), copy, "back to the server's copy");
    assert!(c.body(family, "e1.ics").unwrap().contains("TRIGGER:-PT15M"));
    assert!(!objects::get(&db.pool, anna, family, "e1.ics").await.unwrap().body.contains("VALARM"));
    assert!(matches!(c.store.conflicts().unwrap()[0].detail, ConflictDetail::Rejected { .. }));

    // The network goes away half way: everything applied so far stays,
    // the edit stays pending, and the next run finishes the job.
    let a = phone(&db, &app, &base, anna, "A").await;
    a.synced().await;
    a.edit(family, "e2.ics", Some(&event("e2", "SUMMARY:offline edit\r\n")));
    *a.http.fail_after.lock().unwrap() = Some(a.requests() + 1);
    assert!(matches!(a.sync().await, Err(calzone_client::Error::Network(_))));
    assert_eq!(a.waiting(), 1);
    assert!(a.body(family, "e2.ics").unwrap().contains("offline edit"), "the last copy is kept");
    *a.http.fail_after.lock().unwrap() = None;
    let r = a.synced().await;
    assert_eq!(r.pushed, 1);
    assert!(objects::get(&db.pool, anna, family, "e2.ics").await.unwrap().body.contains("offline edit"));

    // The server restored from an older backup: late.ics, written after
    // it, is gone and the change log is back where it was. Restored as the
    // backup had it, then `calzone-server resync` (deploy/README.md).
    let before: i64 = sqlx::query_scalar("SELECT sequence FROM collections WHERE id = $1")
        .bind(family)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    objects::put(&db.pool, anna, family, "late.ics", &event("late", ""), Precondition::None).await.unwrap();
    a.synced().await;
    assert!(a.body(family, "late.ics").is_some());
    sqlx::query("DELETE FROM objects WHERE collection_id = $1 AND name = 'late.ics'")
        .bind(family)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM changes WHERE collection_id = $1 AND sequence > $2")
        .bind(family)
        .bind(before)
        .execute(&db.pool)
        .await
        .unwrap();
    sqlx::query("UPDATE collections SET sequence = $2 WHERE id = $1")
        .bind(family)
        .bind(before)
        .execute(&db.pool)
        .await
        .unwrap();
    let out = calzone_server::cli::run(&db.pool, &["resync"]).await.unwrap();
    assert!(out.starts_with("1 collections renumbered"), "{out}");
    let old_token = before;
    assert!(
        matches!(
            objects::changes_since(&db.pool, anna, family, Some(old_token)).await,
            Err(calzone_server::domain::DomainError::BadSyncToken)
        ),
        "DAV clients start over too"
    );
    objects::put(&db.pool, anna, family, "e0.ics", &event("e0", "SUMMARY:after restore\r\n"), Precondition::None)
        .await
        .unwrap();
    let r = a.synced().await;
    assert!(r.pulled > 0);
    assert_eq!(a.body(family, "late.ics"), None, "a full sync drops what the server no longer has");
    assert!(a.body(family, "e0.ics").unwrap().contains("after restore"));
    let names: Vec<String> = a.store.objects(&family.to_string()).unwrap().into_iter().map(|o| o.name).collect();
    assert_eq!(names, ["e0.ics", "e1.ics", "e2.ics"]);

    // Removed on the Devices page: the engine says "sign in again".
    let id = calzone_server::domain::devices::list(&db.pool, anna).await.unwrap()[0].id;
    calzone_server::domain::devices::remove(&db.pool, anna, id).await.unwrap();
    assert!(matches!(a.sync().await, Err(calzone_client::Error::SignedOut)));
    // Signing in again to the same account keeps the store.
    a.edit(family, "e0.ics", Some(&event("e0", "SUMMARY:kept\r\n")));
    let q = a.clone();
    let r = send(&app, "POST", "/devices/link", None, &[("cookie", &web_cookie(&db, anna).await)], "").await;
    let at = r.body.find("czl_").unwrap();
    let code = r.body[at..at + 47].to_string();
    let b2 = base.clone();
    let signed = tokio::task::spawn_blocking(move || engine::sign_in(&q.store, q.http.as_ref(), &b2, &code, "A"))
        .await
        .unwrap()
        .unwrap();
    *a.http.token.lock().unwrap() = Some(signed.token);
    assert_eq!(a.waiting(), 1, "the waiting edit survived");
    a.synced().await;
    assert!(objects::get(&db.pool, anna, family, "e0.ics").await.unwrap().body.contains("kept"));
    db.drop().await;
}
