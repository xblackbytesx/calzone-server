//! The calendar page's JSON API against a real Postgres (phase 2 step 2):
//! occurrences with per-person settings, writes in every recurring scope,
//! stale-edit refusal, undo, reminders that stay personal through other
//! people's edits, quick add in both languages, settings.

mod common;

use axum::Router;
use axum::http::StatusCode;
use calzone_server::domain::{Access, CollectionKind, NewCollection, accounts, collections, objects, principals};
use common::{Db, send};
use serde_json::{Value, json};
use uuid::Uuid;

struct Who {
    app: Router,
    cookie: String,
    lang: Option<&'static str>,
}

impl Who {
    async fn new(db: &Db, app: &Router, name: &str) -> (Who, Uuid) {
        let id = principals::create_user(&db.pool, name, "").await.unwrap();
        let token = accounts::create_session(&db.pool, id, true).await.unwrap();
        (Who { app: app.clone(), cookie: format!("calzone_session={token}"), lang: None }, id)
    }

    async fn call(&self, method: &str, path: &str, body: Option<Value>) -> (StatusCode, Value) {
        let mut h = vec![("cookie", self.cookie.as_str()), ("content-type", "application/json")];
        if let Some(l) = self.lang {
            h.push(("accept-language", l));
        }
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        let r = send(&self.app, method, path, None, &h, &body).await;
        (r.status, serde_json::from_str(&r.body).unwrap_or(Value::Null))
    }

    async fn page(&self, path: &str) -> (StatusCode, String) {
        let mut h = vec![("cookie", self.cookie.as_str())];
        if let Some(l) = self.lang {
            h.push(("accept-language", l));
        }
        let r = send(&self.app, "GET", path, None, &h, "").await;
        (r.status, r.body)
    }

    async fn get(&self, path: &str) -> (StatusCode, Value) {
        self.call("GET", path, None).await
    }

    async fn post(&self, path: &str, body: Value) -> (StatusCode, Value) {
        self.call("POST", path, Some(body)).await
    }

    async fn month(&self) -> Vec<Value> {
        let (s, v) = self.get("/api/occurrences?from=2026-01-01&to=2026-03-01").await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v.as_array().unwrap().clone()
    }
}

fn titles(evs: &[Value]) -> Vec<String> {
    evs.iter().map(|e| format!("{} {}", &e["start"].as_str().unwrap()[5..16], e["title"].as_str().unwrap())).collect()
}

async fn family(db: &Db, owner: Uuid) -> Uuid {
    let new = NewCollection {
        kind: CollectionKind::Calendar,
        slug: "family",
        display_name: "Family",
        description: "",
        color: Some("#112233"),
        components: &[],
    };
    collections::create(&db.pool, owner, new).await.unwrap().id
}

/// The served body as `who` reads it.
async fn body(db: &Db, who: Uuid, cal: Uuid, name: &str) -> String {
    objects::get(&db.pool, who, cal, name).await.unwrap().body
}

#[tokio::test]
async fn occurrences_follow_each_persons_settings() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;
    let cal = family(&db, anna_id).await;
    collections::set_grant(&db.pool, anna_id, cal, bob_id, Some(Access::Write)).await.unwrap();
    for w in [&anna, &bob] {
        assert_eq!(w.post("/api/settings", json!({ "timezone": "Europe/Amsterdam" })).await.0, StatusCode::OK);
    }
    let (s, saved) = anna
        .post(
            "/api/events",
            json!({ "collection": cal, "summary": "Swim", "start": "2026-01-05T09:00", "end": "2026-01-05T10:00",
                    "rrule": "FREQ=WEEKLY;COUNT=3", "alarms": [15] }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{saved}");
    let name = saved["name"].as_str().unwrap().to_string();

    let evs = bob.month().await;
    assert_eq!(titles(&evs), ["01-05T09:00 Swim", "01-12T09:00 Swim", "01-19T09:00 Swim"]);
    let e = &evs[1];
    assert_eq!(
        (e["own"].as_bool(), e["owner"].as_str(), e["color"].as_str()),
        (Some(false), Some("anna"), Some("#112233"))
    );
    assert_eq!(
        (e["calendar"].as_str(), e["editable"].as_bool(), e["recurring"].as_bool()),
        (Some("Family"), Some(true), Some(true))
    );
    assert_eq!(e["instance"].as_str(), Some("20260112T090000"));
    assert_eq!(e["id"].as_str().unwrap(), format!("{cal}/{name}/20260112T090000"));
    assert_eq!(anna.month().await[0]["own"].as_bool(), Some(true));

    // Bob's own color and name for Anna's calendar; Anna's view is unchanged.
    let prefs = json!({ "color": "#FF0000", "display_name": "Anna's", "default_alarm": 30 });
    assert_eq!(bob.post(&format!("/api/calendars/{cal}/prefs"), prefs).await.0, StatusCode::OK);
    let e = &bob.month().await[0];
    assert_eq!((e["color"].as_str(), e["calendar"].as_str()), (Some("#ff0000"), Some("Anna's")));
    assert_eq!(anna.month().await[0]["color"].as_str(), Some("#112233"));
    let (_, list) = bob.get("/api/calendars").await;
    assert_eq!(
        (list[0]["name"].as_str(), list[0]["own"].as_bool(), list[0]["default_alarm"].as_i64()),
        (Some("Anna's"), Some(false), Some(30))
    );

    // Another zone shows other wall-clock times; a hidden calendar shows nothing.
    bob.post("/api/settings", json!({ "timezone": "America/New_York" })).await;
    assert_eq!(&bob.month().await[0]["start"], "2026-01-05T03:00:00");
    bob.post(&format!("/api/calendars/{cal}/prefs"), json!({ "hidden": true })).await;
    assert!(bob.month().await.is_empty());

    // Reminders stay personal: only Anna has hers.
    assert!(body(&db, anna_id, cal, &name).await.contains("TRIGGER:-PT15M"));
    assert!(!body(&db, bob_id, cal, &name).await.contains("VALARM"));

    // Bob's default reminder applies to what he creates.
    bob.post(&format!("/api/calendars/{cal}/prefs"), json!({ "default_alarm": 30 })).await;
    let (_, b) =
        bob.post("/api/events", json!({ "collection": cal, "summary": "Dentist", "start": "2026-02-03" })).await;
    let bname = b["name"].as_str().unwrap();
    assert!(body(&db, bob_id, cal, bname).await.contains("TRIGGER:-PT30M"));
    assert!(!body(&db, anna_id, cal, bname).await.contains("VALARM"));

    // Ranges are checked.
    assert_eq!(bob.get("/api/occurrences?from=2026-01-01&to=2028-01-01").await.0, StatusCode::BAD_REQUEST);
    assert_eq!(bob.get("/api/occurrences?from=x&to=y").await.0, StatusCode::BAD_REQUEST);
    db.drop().await;
}

#[tokio::test]
async fn writes_refuse_stale_edits_and_undo() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;
    let (carol, carol_id) = Who::new(&db, &app, "carol").await;
    let cal = family(&db, anna_id).await;
    collections::set_grant(&db.pool, anna_id, cal, bob_id, Some(Access::Write)).await.unwrap();
    collections::set_grant(&db.pool, anna_id, cal, carol_id, Some(Access::Read)).await.unwrap();
    for w in [&anna, &bob, &carol] {
        w.post("/api/settings", json!({ "timezone": "Europe/Amsterdam" })).await;
    }
    let (_, saved) = anna
        .post(
            "/api/events",
            json!({ "collection": cal, "summary": "Swim", "start": "2026-01-05T09:00", "end": "2026-01-05T10:00",
                    "rrule": "FREQ=WEEKLY;COUNT=4", "alarms": [15] }),
        )
        .await;
    let name = saved["name"].as_str().unwrap().to_string();
    let path = format!("/api/events/{cal}/{name}");
    let etag = |evs: &[Value]| evs[0]["etag"].as_str().unwrap().to_string();

    // Only this one, moved.
    let evs = anna.month().await;
    let (s, moved) = anna
        .post(
            &path,
            json!({ "etag": etag(&evs), "instance": "20260112T090000", "scope": "this", "start": "2026-01-13T18:00" }),
        )
        .await;
    assert_eq!(s, StatusCode::OK, "{moved}");
    assert_eq!(
        titles(&anna.month().await),
        ["01-05T09:00 Swim", "01-13T18:00 Swim", "01-19T09:00 Swim", "01-26T09:00 Swim"]
    );
    // The old ETag is stale now.
    let (s, err) = anna.post(&path, json!({ "etag": etag(&evs), "summary": "Overwrite" })).await;
    assert_eq!(s, StatusCode::PRECONDITION_FAILED);
    assert!(err["error"].as_str().unwrap().contains("changed somewhere else"));
    // Undo, once.
    let undo = format!("/api/undo/{}", moved["change_id"].as_str().unwrap());
    assert_eq!(anna.post(&undo, json!({})).await.0, StatusCode::OK);
    assert_eq!(titles(&anna.month().await)[1], "01-12T09:00 Swim");
    assert_eq!(anna.post(&undo, json!({})).await.0, StatusCode::NOT_FOUND);
    // Someone else's change cannot be undone by Bob.
    let evs = anna.month().await;
    let (_, del) = anna
        .post(
            &format!("{path}/delete"),
            json!({ "etag": etag(&evs), "instance": "20260119T090000", "scope": "following" }),
        )
        .await;
    assert_eq!(anna.month().await.len(), 2);
    assert_eq!(
        bob.post(&format!("/api/undo/{}", del["change_id"].as_str().unwrap()), json!({})).await.0,
        StatusCode::NOT_FOUND
    );
    anna.post(&format!("/api/undo/{}", del["change_id"].as_str().unwrap()), json!({})).await;
    assert_eq!(anna.month().await.len(), 4);

    // Bob splits Anna's series: Anna keeps her reminder on both parts.
    let evs = bob.month().await;
    let (s, split) = bob
        .post(&path, json!({ "etag": etag(&evs), "instance": "20260119T090000", "scope": "following", "summary": "Swim club", "alarms": [5] }))
        .await;
    assert_eq!(s, StatusCode::OK, "{split}");
    let created = split["created"].as_str().unwrap().to_string();
    assert_eq!(
        titles(&anna.month().await),
        ["01-05T09:00 Swim", "01-12T09:00 Swim", "01-19T09:00 Swim club", "01-26T09:00 Swim club"]
    );
    for n in [&name, &created] {
        let a = body(&db, anna_id, cal, n).await;
        assert!(a.contains("TRIGGER:-PT15M") && !a.contains("TRIGGER:-PT5M"), "Anna's reminder on {n}");
    }
    assert!(body(&db, bob_id, cal, &created).await.contains("TRIGGER:-PT5M"));
    // Each write took its own sequence number, so DAV clients see both.
    let seqs: Vec<i64> = sqlx::query_scalar("SELECT sequence FROM changes WHERE collection_id = $1 ORDER BY sequence")
        .bind(cal)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    let mut dedup = seqs.clone();
    dedup.dedup();
    assert_eq!(seqs, dedup, "no sequence number used twice");
    // Undo the split: one object again, Anna's reminder intact.
    assert_eq!(
        bob.post(&format!("/api/undo/{}", split["change_id"].as_str().unwrap()), json!({})).await.0,
        StatusCode::OK
    );
    assert_eq!(objects::list(&db.pool, anna_id, cal).await.unwrap().len(), 1);
    assert_eq!(titles(&anna.month().await)[3], "01-26T09:00 Swim");
    assert!(body(&db, anna_id, cal, &name).await.contains("TRIGGER:-PT15M"));

    // An undo after a later change is refused.
    let evs = anna.month().await;
    let (_, c1) = anna.post(&path, json!({ "etag": etag(&evs), "summary": "One" })).await;
    let (_, c2) = anna.post(&path, json!({ "etag": c1["etag"].as_str().unwrap(), "summary": "Two" })).await;
    assert_eq!(
        anna.post(&format!("/api/undo/{}", c1["change_id"].as_str().unwrap()), json!({})).await.0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        anna.post(&format!("/api/undo/{}", c2["change_id"].as_str().unwrap()), json!({})).await.0,
        StatusCode::OK
    );
    assert_eq!(anna.month().await[0]["title"].as_str(), Some("One"));

    // Deleting all and undoing brings it back with Anna's reminder.
    let evs = anna.month().await;
    let (_, gone) = anna.post(&format!("{path}/delete"), json!({ "etag": etag(&evs), "scope": "all" })).await;
    assert!(gone["etag"].is_null());
    assert!(anna.month().await.is_empty());
    bob.post(&format!("/api/undo/{}", gone["change_id"].as_str().unwrap()), json!({})).await;
    assert!(anna.month().await.is_empty(), "Bob cannot undo Anna's delete");
    anna.post(&format!("/api/undo/{}", gone["change_id"].as_str().unwrap()), json!({})).await;
    assert_eq!(anna.month().await.len(), 4);
    assert!(body(&db, anna_id, cal, &name).await.contains("TRIGGER:-PT15M"));

    // Read-only and invisible.
    let evs = carol.month().await;
    assert_eq!(evs[0]["editable"].as_bool(), Some(false));
    assert_eq!(carol.post(&path, json!({ "etag": etag(&evs), "summary": "x" })).await.0, StatusCode::FORBIDDEN);
    let (dave, _) = Who::new(&db, &app, "dave").await;
    assert_eq!(dave.post(&path, json!({ "etag": etag(&evs), "summary": "x" })).await.0, StatusCode::NOT_FOUND);
    assert_eq!(
        dave.post("/api/events", json!({ "collection": cal, "summary": "x", "start": "2026-01-01" })).await.0,
        StatusCode::NOT_FOUND
    );
    // Bad input.
    let (s, e) = anna.post(&path, json!({ "etag": etag(&evs), "start": "soon" })).await;
    assert_eq!((s, e["error"].as_str()), (StatusCode::BAD_REQUEST, Some("The start is not a date or time.")));
    assert_eq!(
        anna.post(&path, json!({ "etag": etag(&evs), "instance": "20260112T100000", "scope": "this", "summary": "x" }))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(anna.post(&path, json!({ "etag": etag(&evs), "scope": "sometimes" })).await.0, StatusCode::BAD_REQUEST);
    // Signed out: JSON, not a redirect.
    let r = send(&app, "GET", "/api/occurrences?from=2026-01-01&to=2026-02-01", None, &[], "").await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert!(r.body.contains("\"error\""));
    db.drop().await;
}

#[tokio::test]
async fn quick_add_and_settings() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (mut anna, _) = Who::new(&db, &app, "anna").await;
    anna.post("/api/settings", json!({ "timezone": "Europe/Amsterdam" })).await;
    let (s, d) = anna.post("/api/quickadd", json!({ "text": "dentist thu 3pm" })).await;
    assert_eq!(s, StatusCode::OK, "{d}");
    assert_eq!((d["title"].as_str(), d["all_day"].as_bool()), (Some("dentist"), Some(false)));
    assert!(d["start"].as_str().unwrap().ends_with("T15:00:00"));
    // The browser's language until a setting says otherwise.
    anna.lang = Some("nl-NL,nl;q=0.9");
    let (_, d) = anna.post("/api/quickadd", json!({ "text": "tandarts do 15u" })).await;
    assert_eq!(d["title"].as_str(), Some("tandarts"));
    let (_, v) = anna.get("/api/settings").await;
    assert_eq!(
        (v["language"].as_str(), v["language_set"].as_bool(), v["theme"].as_str()),
        (Some("nl"), Some(false), Some("dark"))
    );
    anna.post("/api/settings", json!({ "language": "en" })).await;
    let (_, d) = anna.post("/api/quickadd", json!({ "text": "tandarts do 15u" })).await;
    assert_eq!(d["start"], Value::Null, "English now: no Dutch words");
    anna.post("/api/settings", json!({ "language": null })).await;
    assert_eq!(anna.get("/api/settings").await.1["language_set"].as_bool(), Some(false));
    let (_, d) = anna.post("/api/quickadd", json!({ "text": "vakantie 24-28 dec" })).await;
    assert_eq!((d["all_day"].as_bool(), d["end"].as_str()), (Some(true), Some("2026-12-29")));
    assert_eq!(anna.post("/api/quickadd", json!({ "text": "morgen 15u" })).await.0, StatusCode::BAD_REQUEST);

    // Settings are validated.
    for bad in [
        json!({ "timezone": "Mars/Base" }),
        json!({ "theme": "pink" }),
        json!({ "language": "de" }),
        json!({ "week_start": 9 }),
    ] {
        assert_eq!(anna.post("/api/settings", bad.clone()).await.0, StatusCode::BAD_REQUEST, "{bad}");
    }
    anna.post("/api/settings", json!({ "theme": "light", "week_start": 7, "clock_24h": false })).await;
    let (_, v) = anna.get("/api/settings").await;
    assert_eq!(
        (v["theme"].as_str(), v["week_start"].as_i64(), v["clock_24h"].as_bool()),
        (Some("light"), Some(7), Some(false))
    );
    assert_eq!((v["timezone"].as_str(), v["timezone_set"].as_bool()), (Some("Europe/Amsterdam"), Some(true)));
    db.drop().await;
}

#[tokio::test]
async fn the_calendar_page_and_its_panel() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (mut anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;
    let cal = family(&db, anna_id).await;
    collections::set_grant(&db.pool, anna_id, cal, bob_id, Some(Access::Read)).await.unwrap();
    for w in [&anna, &bob] {
        w.post("/api/settings", json!({ "timezone": "Europe/Amsterdam" })).await;
    }
    let (s, page) = anna.page("/calendar").await;
    assert_eq!(s, StatusCode::OK);
    assert!(page.contains(r#"<html lang="en" data-theme="dark">"#), "dark and English by default");
    assert!(page.contains("/static/event-calendar.min.js") && page.contains("data-view=\"timeGridWeek\""));
    // Month and days (decision 54): first in the switcher, with its own small month.
    assert!(page.contains(r#"data-view="monthDays">Month and days<"#) && page.contains(r#"id="cal-mini""#));
    assert!(page.contains("Add: dentist thu 3pm") && page.contains("&#34;first_day&#34;:1"));
    // The browser's language until set; a theme setting reaches <html>.
    anna.lang = Some("nl-NL,nl;q=0.9");
    anna.post("/api/settings", json!({ "theme": "light" })).await;
    let (_, page) = anna.page("/calendar").await;
    assert!(page.contains(r#"<html lang="nl" data-theme="light">"#));
    assert!(page.contains("Toevoegen: tandarts do 15u") && page.contains(">Vandaag<") && page.contains(">Agenda<"));
    assert!(page.contains(">Maand en dagen<"));
    assert!(page.contains("&#34;locale&#34;:&#34;nl-NL&#34;"));

    // The panel for an occurrence: its own time, Anna's reminder only.
    let (_, saved) = anna
        .post(
            "/api/events",
            json!({ "collection": cal, "summary": "Swim & sauna", "start": "2026-01-05T09:00", "end": "2026-01-05T10:30",
                    "rrule": "FREQ=WEEKLY", "alarms": [15], "location": "Pool" }),
        )
        .await;
    let name = saved["name"].as_str().unwrap();
    let path = format!("/calendar/event?c={cal}&n={name}&i=20260112T090000");
    let (s, panel) = anna.page(&path).await;
    assert_eq!(s, StatusCode::OK, "{panel}");
    assert!(
        panel.contains(r#"name="start_date" value="2026-01-12""#)
            && panel.contains(r#"name="start_time" value="09:00""#)
    );
    assert!(panel.contains(r#"name="end_time" value="10:30""#) && panel.contains("Swim &#38; sauna"));
    assert!(panel.contains(r#"<option value="weekly" selected>"#) && panel.contains(r#"<option value="15" selected>"#));
    assert!(panel.contains(r#"data-recurring="1""#) && panel.contains(r#"data-instance="20260112T090000""#));
    assert!(panel.contains("Alleen jij krijgt deze."), "Dutch in the panel too");
    // Bob (read only): no reminder of Anna's, a disabled form, no save button.
    let (_, panel) = bob.page(&path).await;
    assert!(
        panel.contains(r#"<option value="none" selected>"#)
            && panel.contains("fieldset class=\"sheet-fields\" disabled")
    );
    assert!(
        panel.contains("Save my reminder") && panel.contains("From anna") && !panel.contains("data-cal=\"delete\"")
    );
    // A new event at a grid slot; all-day ranges show their last day.
    let (_, panel) = anna.page("/calendar/event/new?start=2026-01-07T14:00").await;
    assert!(panel.contains(r#"name="start_time" value="14:00""#) && panel.contains(r#"name="end_time" value="15:00""#));
    assert!(panel.contains(&format!(r#"<option value="{cal}">"#)));
    let (_, panel) = anna.page("/calendar/event/new?start=2026-01-07&end=2026-01-10&all_day=1").await;
    assert!(panel.contains(r#"name="end_date" value="2026-01-09""#) && panel.contains("value=\"1\" checked"));
    // Bob cannot add to a read-only calendar, and has no other.
    let (s, _) = bob.page("/calendar/event/new").await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (dave, _) = Who::new(&db, &app, "dave").await;
    assert_eq!(dave.page(&path).await.0, StatusCode::NOT_FOUND);
    // The old home goes to the calendar.
    let r = send(&app, "GET", "/", None, &[("cookie", anna.cookie.as_str())], "").await;
    assert_eq!(r.header("location").as_deref(), Some("/calendar"));
    db.drop().await;
}

#[tokio::test]
async fn readers_set_their_own_reminders() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (bob, bob_id) = Who::new(&db, &app, "bob").await;
    let cal = family(&db, anna_id).await;
    collections::set_grant(&db.pool, anna_id, cal, bob_id, Some(Access::Read)).await.unwrap();
    for w in [&anna, &bob] {
        w.post("/api/settings", json!({ "timezone": "Europe/Amsterdam" })).await;
    }
    let (_, saved) = anna
        .post("/api/events", json!({ "collection": cal, "summary": "Swim", "start": "2026-01-05T09:00", "rrule": "FREQ=WEEKLY", "alarms": [15] }))
        .await;
    let name = saved["name"].as_str().unwrap().to_string();
    let path = format!("/api/events/{cal}/{name}");
    let shared = || async {
        sqlx::query_scalar::<_, String>("SELECT body FROM objects WHERE name = $1")
            .bind(&name)
            .fetch_one(&db.pool)
            .await
            .unwrap()
    };
    let before = shared().await;
    let evs = bob.month().await;
    let etag = evs[0]["etag"].as_str().unwrap();
    // Bob can only view, yet sets his reminder (an occurrence named: still every one).
    let (s, r) =
        bob.post(&path, json!({ "etag": etag, "instance": "20260112T090000", "scope": "this", "alarms": [60] })).await;
    assert_eq!(s, StatusCode::OK, "{r}");
    assert_eq!(shared().await, before, "the shared event is untouched");
    let b = body(&db, bob_id, cal, &name).await;
    assert!(b.contains("TRIGGER:-PT1H") && !b.contains("PT15M"));
    let a = body(&db, anna_id, cal, &name).await;
    assert!(a.contains("TRIGGER:-PT15M") && !a.contains("PT1H"), "Anna keeps hers");
    // Anything more than reminders is still refused.
    let etag2 = r["etag"].as_str().unwrap();
    assert_eq!(
        bob.post(&path, json!({ "etag": etag2, "summary": "Mine", "alarms": [5] })).await.0,
        StatusCode::FORBIDDEN
    );
    // Anna's later edit keeps Bob's reminder; Bob can undo his.
    let (_, ed) = anna.post(&path, json!({ "etag": etag2, "summary": "Swim club" })).await;
    assert!(ed["etag"].is_string());
    assert!(body(&db, bob_id, cal, &name).await.contains("TRIGGER:-PT1H"));
    let (_, again) = bob.post(&path, json!({ "etag": ed["etag"], "alarms": [] })).await;
    assert!(!body(&db, bob_id, cal, &name).await.contains("VALARM"));
    assert_eq!(
        bob.post(&format!("/api/undo/{}", again["change_id"].as_str().unwrap()), json!({})).await.0,
        StatusCode::OK
    );
    assert!(body(&db, bob_id, cal, &name).await.contains("TRIGGER:-PT1H"));
    db.drop().await;
}
