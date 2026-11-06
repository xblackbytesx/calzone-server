//! ICS feeds against a real Postgres (decision 41): secret links serve one
//! calendar at full or busy-only detail and stop at revocation; a
//! subscription reads a feed over HTTP (our own published link, and a
//! source we can break), keeps the last good copy, stays read-only, and
//! keeps people's own reminders; the guard refuses private addresses.

mod common;

use std::sync::{Arc, Mutex};

use axum::Router;
use axum::http::StatusCode;
use axum::routing::get;
use calzone_server::domain::{
    CollectionKind, DomainError, NewCollection, Precondition, accounts, collections, objects, principals,
};
use calzone_server::{AppState, router};
use common::{Db, send};
use serde_json::{Value, json};
use uuid::Uuid;

struct Who {
    app: Router,
    cookie: String,
}

impl Who {
    async fn new(db: &Db, app: &Router, name: &str) -> (Who, Uuid) {
        let id = principals::create_user(&db.pool, name, "").await.unwrap();
        let token = accounts::create_session(&db.pool, id, true).await.unwrap();
        (Who { app: app.clone(), cookie: format!("calzone_session={token}") }, id)
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        let h = [("cookie", self.cookie.as_str()), ("content-type", "application/json")];
        let r = send(&self.app, "POST", path, None, &h, &body.to_string()).await;
        (r.status, serde_json::from_str(&r.body).unwrap_or(Value::Null))
    }

    async fn get(&self, path: &str) -> (StatusCode, String) {
        let r = send(&self.app, "GET", path, None, &[("cookie", self.cookie.as_str())], "").await;
        (r.status, r.body)
    }
}

fn event(uid: &str, summary: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
         DTSTART:20260105T090000Z\r\nDTEND:20260105T100000Z\r\nSUMMARY:{summary}\r\nLOCATION:Clinic\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

async fn calendar(db: &Db, owner: Uuid, slug: &str) -> Uuid {
    let new = NewCollection {
        kind: CollectionKind::Calendar,
        slug,
        display_name: "Family",
        description: "",
        color: None,
        components: &[],
    };
    collections::create(&db.pool, owner, new).await.unwrap().id
}

async fn put(db: &Db, who: Uuid, cal: Uuid, name: &str, body: &str) {
    objects::put(&db.pool, who, cal, name, body, Precondition::None).await.unwrap();
}

/// The path of a link's feed (`/feeds/<token>.ics`) from its https URL.
fn feed_path(link: &Value) -> String {
    let url = link["url"].as_str().unwrap();
    url[url.find("/feeds/").unwrap()..].to_string()
}

#[tokio::test]
async fn links_serve_full_or_busy_and_stop_when_revoked() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;
    let cal = calendar(&db, anna_id, "family").await;
    let alarm = "BEGIN:VALARM\r\nACTION:DISPLAY\r\nTRIGGER:-PT15M\r\nDESCRIPTION:x\r\nEND:VALARM\r\n";
    put(&db, anna_id, cal, "a.ics", &event("a", "Therapy", alarm)).await;
    put(&db, anna_id, cal, "b.ics", &event("b", "Diary", "CLASS:PRIVATE\r\n")).await;
    put(&db, anna_id, cal, "c.ics", &event("c", "Lunch maybe", "TRANSP:TRANSPARENT\r\n")).await;

    // Only someone who may share it makes links.
    assert_eq!(
        bob.post(&format!("/api/calendars/{cal}/links"), json!({ "detail": "full" })).await.0,
        StatusCode::NOT_FOUND
    );
    let (s, full) =
        anna.post(&format!("/api/calendars/{cal}/links"), json!({ "label": "Grandma", "detail": "full" })).await;
    assert_eq!(s, StatusCode::OK, "{full}");
    assert!(full["webcal"].as_str().unwrap().starts_with("webcal://"));
    let (_, busy) = anna.post(&format!("/api/calendars/{cal}/links"), json!({ "detail": "busy" })).await;

    // Full: titles and places, but a private event is busy-only, and no
    // one's reminders. No session needed.
    let r = send(&app, "GET", &feed_path(&full), None, &[], "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.header("content-type").as_deref(), Some("text/calendar; charset=utf-8"));
    assert!(r.body.contains("SUMMARY:Therapy") && r.body.contains("LOCATION:Clinic") && r.body.contains("Lunch maybe"));
    assert!(!r.body.contains("Diary") && r.body.contains("SUMMARY:Busy"));
    assert!(!r.body.contains("VALARM"), "reminders are personal");
    let etag = r.header("etag").unwrap();
    assert!(r.header("last-modified").unwrap().ends_with(" GMT"));
    let again = send(&app, "GET", &feed_path(&full), None, &[("if-none-match", &etag)], "").await;
    assert_eq!(again.status, StatusCode::NOT_MODIFIED);
    put(&db, anna_id, cal, "d.ics", &event("d", "Dentist", "")).await;
    let changed = send(&app, "GET", &feed_path(&full), None, &[("if-none-match", &etag)], "").await;
    assert_eq!(changed.status, StatusCode::OK, "a write changes the ETag");

    // Busy: times only, free time left out.
    let r = send(&app, "GET", &feed_path(&busy), None, &[], "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("DTSTART:20260105T090000Z"));
    for gone in ["Therapy", "Clinic", "Diary", "Lunch maybe", "Dentist"] {
        assert!(!r.body.contains(gone), "{gone}");
    }
    assert_eq!(r.body.matches("BEGIN:VEVENT").count(), 3);
    // In the maker's language.
    sqlx::query("UPDATE principals SET language = 'nl' WHERE id = $1").bind(anna_id).execute(&db.pool).await.unwrap();
    assert!(send(&app, "GET", &feed_path(&busy), None, &[], "").await.body.contains("SUMMARY:Bezet"));

    // The sheet lists the links (never the token) and Bob sees none.
    let (_, sheet) = anna.get(&format!("/calendar/settings/{cal}")).await;
    assert!(sheet.contains("Grandma") && sheet.contains("id=\"cal-links\""));
    let token = feed_path(&full).trim_start_matches("/feeds/").trim_end_matches(".ics").to_string();
    assert!(!sheet.contains(&token));
    collections::set_grant(&db.pool, anna_id, cal, bob_id, Some(calzone_server::domain::Access::Read)).await.unwrap();
    assert!(!bob.get(&format!("/calendar/settings/{cal}")).await.1.contains("cal-links"));
    assert_eq!(
        bob.post(&format!("/api/links/{}/revoke", full["id"].as_str().unwrap()), json!({})).await.0,
        StatusCode::FORBIDDEN
    );

    // Revoked, unknown and made-up tokens are all the same 404; the stored
    // value is a hash, not the token.
    assert_eq!(
        anna.post(&format!("/api/links/{}/revoke", full["id"].as_str().unwrap()), json!({})).await.0,
        StatusCode::OK
    );
    assert_eq!(send(&app, "GET", &feed_path(&full), None, &[], "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(send(&app, "GET", "/feeds/nope.ics", None, &[], "").await.status, StatusCode::NOT_FOUND);
    let stored: Vec<Vec<u8>> =
        sqlx::query_scalar("SELECT token_sha256 FROM feed_links").fetch_all(&db.pool).await.unwrap();
    assert!(stored.iter().all(|h| h.len() == 32 && h != token.as_bytes()));
    // A disabled owner's feeds stop too.
    principals::set_disabled(&db.pool, anna_id, true).await.unwrap();
    assert_eq!(send(&app, "GET", &feed_path(&busy), None, &[], "").await.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

/// A feed source we control: status and body.
async fn source(state: Arc<Mutex<(u16, String)>>) -> String {
    let app = Router::new().route(
        "/feed.ics",
        get(move || {
            let state = state.clone();
            async move {
                let (status, body) = state.lock().unwrap().clone();
                (StatusCode::from_u16(status).unwrap(), body)
            }
        }),
    );
    common::serve(app).await
}

fn local_state(db: &Db) -> AppState {
    AppState { feeds_allow_private: true, ..db.state() }
}

#[tokio::test]
async fn subscriptions_follow_the_source_and_keep_the_last_good_copy() {
    let Some(db) = Db::new().await else { return };
    let app = router(local_state(&db));
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;

    // Bob subscribes to Anna's published link, served over real HTTP.
    let family = calendar(&db, anna_id, "family").await;
    put(&db, anna_id, family, "a.ics", &event("a", "Swim", "")).await;
    let (_, link) = anna.post(&format!("/api/calendars/{family}/links"), json!({ "detail": "full" })).await;
    let base = common::serve(app.clone()).await;
    let (s, sub) =
        bob.post("/api/subscriptions", json!({ "url": format!("{base}{}", feed_path(&link)), "interval": 15 })).await;
    assert_eq!(s, StatusCode::OK, "{sub}");
    let copy = Uuid::parse_str(sub["id"].as_str().unwrap()).unwrap();
    let c = collections::get(&db.pool, bob_id, copy).await.unwrap();
    assert!(c.subscribed && c.display_name == "Family", "named after the feed");
    let bodies = |who: Uuid, cal: Uuid| {
        let pool = db.pool.clone();
        async move { objects::all(&pool, who, cal).await.unwrap().into_iter().map(|s| s.body).collect::<Vec<_>>() }
    };
    assert!(bodies(bob_id, copy).await.iter().any(|b| b.contains("SUMMARY:Swim")));

    // Read-only for Bob: the store, the event API, and the page say so.
    let e = objects::put(&db.pool, bob_id, copy, "x.ics", &event("x", "Mine", ""), Precondition::None).await;
    assert!(matches!(e, Err(DomainError::ReadOnly)), "{e:?}");
    let (s, _) =
        bob.post("/api/events", json!({ "collection": copy, "summary": "Mine", "start": "2026-01-05T09:00" })).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (_, sheet) = bob.get(&format!("/calendar/settings/{copy}")).await;
    assert!(sheet.contains("id=\"cal-sub\"") && sheet.contains("127.0.0.1") && !sheet.contains("cal-links"));
    assert!(bob.get("/calendar").await.1.contains("cal-sub-mark"));

    // But Bob's own reminders work, and survive a changed refresh.
    let name = objects::all(&db.pool, bob_id, copy).await.unwrap()[0].meta.name.clone();
    let etag = objects::get(&db.pool, bob_id, copy, &name).await.unwrap().meta.etag();
    let (s, r) = bob.post(&format!("/api/events/{copy}/{name}"), json!({ "etag": etag, "alarms": [30] })).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    put(&db, anna_id, family, "a.ics", &event("a", "Swim club", "")).await;
    put(&db, anna_id, family, "b.ics", &event("b", "Choir", "")).await;
    let (s, r) = bob.post(&format!("/api/subscriptions/{copy}/refresh"), json!({})).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!((r["added"].as_u64(), r["updated"].as_u64()), (Some(1), Some(1)));
    let swim = objects::get(&db.pool, bob_id, copy, &name).await.unwrap().body;
    assert!(swim.contains("Swim club") && swim.contains("TRIGGER:-PT30M"), "his reminder stays");
    // Nothing changed since: the ETag says so.
    let (_, r) = bob.post(&format!("/api/subscriptions/{copy}/refresh"), json!({})).await;
    assert_eq!(r["changed"], json!(false));
    // A removal at the source is a removal here.
    objects::delete(&db.pool, anna_id, family, "b.ics", Precondition::None).await.unwrap();
    let (_, r) = bob.post(&format!("/api/subscriptions/{copy}/refresh"), json!({})).await;
    assert_eq!(r["removed"].as_u64(), Some(1));

    // A source that breaks, empties or sends rubbish: the copy stays.
    let state = Arc::new(Mutex::new((
        200,
        format!(
            "BEGIN:VCALENDAR\r\nX-WR-CALNAME:Holidays\r\n{}END:VCALENDAR\r\n",
            "BEGIN:VEVENT\r\nUID:h1\r\nDTSTART;VALUE=DATE:20261225\r\nSUMMARY:Christmas\r\nEND:VEVENT\r\n"
        ),
    )));
    let src = source(state.clone()).await;
    let (s, h) = bob.post("/api/subscriptions", json!({ "url": format!("{src}/feed.ics"), "name": "School" })).await;
    assert_eq!(s, StatusCode::OK, "{h}");
    let hol = Uuid::parse_str(h["id"].as_str().unwrap()).unwrap();
    assert_eq!(collections::get(&db.pool, bob_id, hol).await.unwrap().display_name, "School");
    let good = bodies(bob_id, hol).await;
    for (status, body, why) in [
        (500, "oops".to_string(), "HTTP 500"),
        (200, "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n".to_string(), "no events"),
        (200, "<html>login</html>".to_string(), "iCalendar"),
        // One good entry and one the store refuses (an event and a task
        // under one UID): the whole copy is refused.
        (200, "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:ok\r\nDTSTART:20260101T090000Z\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:z\r\nDTSTART:20260101T090000Z\r\nEND:VEVENT\r\nBEGIN:VTODO\r\nUID:z\r\nEND:VTODO\r\nEND:VCALENDAR\r\n".to_string(), "Entry 2"),
    ] {
        *state.lock().unwrap() = (status, body);
        let (s, r) = bob.post(&format!("/api/subscriptions/{hol}/refresh"), json!({})).await;
        assert_eq!(s, StatusCode::UNPROCESSABLE_ENTITY, "{why}: {r}");
        assert!(r["error"].as_str().unwrap().contains(why), "{why}: {r}");
        assert_eq!(bodies(bob_id, hol).await, good, "{why}: last good copy kept");
    }
    // The failure shows on the sheet in Bob's language, with backoff.
    sqlx::query("UPDATE principals SET language = 'nl' WHERE id = $1").bind(bob_id).execute(&db.pool).await.unwrap();
    let (_, sheet) = bob.get(&format!("/calendar/settings/{hol}")).await;
    assert!(sheet.contains("onleesbaar"), "translated error");
    let failures: i32 = sqlx::query_scalar("SELECT failures FROM subscriptions WHERE collection_id = $1")
        .bind(hol)
        .fetch_one(&db.pool)
        .await
        .unwrap();
    assert_eq!(failures, 4);
    // A failing address never makes a calendar.
    let before = collections::visible(&db.pool, bob_id, CollectionKind::Calendar).await.unwrap().len();
    *state.lock().unwrap() = (404, String::new());
    let (s, r) = bob.post("/api/subscriptions", json!({ "url": format!("{src}/feed.ics") })).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert!(r["error"].as_str().unwrap().contains("HTTP 404"), "{r}");
    assert_eq!(collections::visible(&db.pool, bob_id, CollectionKind::Calendar).await.unwrap().len(), before);

    // Interval and unsubscribing are Bob's; Anna cannot see them.
    assert_eq!(
        bob.post(&format!("/api/subscriptions/{hol}/interval"), json!({ "minutes": 1440 })).await.0,
        StatusCode::OK
    );
    assert_eq!(
        bob.post(&format!("/api/subscriptions/{hol}/interval"), json!({ "minutes": 7 })).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(anna.post(&format!("/api/subscriptions/{hol}/refresh"), json!({})).await.0, StatusCode::NOT_FOUND);
    assert_eq!(bob.post(&format!("/api/subscriptions/{hol}/delete"), json!({})).await.0, StatusCode::OK);
    assert!(collections::get(&db.pool, bob_id, hol).await.is_err());
    db.drop().await;
}

#[tokio::test]
async fn private_addresses_are_refused_unless_allowed() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;
    let state = Arc::new(Mutex::new((200, event("a", "x", ""))));
    let src = source(state).await;
    for url in [format!("{src}/feed.ics"), "http://localhost/feed.ics".into(), "webcal://169.254.169.254/latest".into()]
    {
        let (s, r) = bob.post("/api/subscriptions", json!({ "url": url })).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{url}");
        assert!(r["error"].as_str().unwrap().contains("private network"), "{url}: {r}");
    }
    for url in ["ftp://example.org/f.ics", "https://me:pw@example.org/f.ics"] {
        assert_eq!(bob.post("/api/subscriptions", json!({ "url": url })).await.0, StatusCode::BAD_REQUEST, "{url}");
    }
    assert!(collections::visible(&db.pool, bob_id, CollectionKind::Calendar).await.unwrap().is_empty());
    // Dutch for a Dutch reader.
    sqlx::query("UPDATE principals SET language = 'nl' WHERE id = $1").bind(bob_id).execute(&db.pool).await.unwrap();
    let (_, r) = bob.post("/api/subscriptions", json!({ "url": format!("{src}/feed.ics") })).await;
    assert!(r["error"].as_str().unwrap().contains("privénetwerk"), "{r}");
    db.drop().await;
}
