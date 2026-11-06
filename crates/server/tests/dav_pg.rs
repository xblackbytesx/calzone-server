//! The CalDAV/CardDAV door end to end against a real Postgres: discovery
//! and sync the way DAVx5 and Thunderbird do it, conditional writes,
//! personal alarms, sharing, scoped app passwords, collection management,
//! and the door switched off.

mod common;

use std::collections::HashMap;

use axum::Router;
use axum::http::StatusCode;
use calzone_dav::xml::{self, CALDAV, CARDDAV, CS, DAV, Element, ICAL};
use calzone_server::domain::app_passwords::{self, Scope};
use calzone_server::domain::{Access, CollectionKind, NewCollection, collections, principals};
use common::{Db, Reply, send};
use sqlx::PgPool;
use uuid::Uuid;

const ALARM: &str = "BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n";

fn event(uid: &str, start: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
         SUMMARY:Synthetic & <{uid}>\r\nDTSTART:{start}\r\nDURATION:PT1H\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

fn todo(uid: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\nBEGIN:VTODO\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
         SUMMARY:Task\r\nEND:VTODO\r\nEND:VCALENDAR\r\n"
    )
}

fn card(uid: &str, name: &str) -> String {
    format!(
        "BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\nN:{name};;;;\r\nEMAIL:{uid}@example.org\r\nEND:VCARD\r\n"
    )
}

struct World {
    db: Db,
    app: Router,
    anna: (Uuid, String),
    bob: (Uuid, String),
    carol: (Uuid, String),
    /// anna's calendar, shared with bob for writing.
    family: Uuid,
    /// anna's private calendar.
    private: Uuid,
    /// carol's calendar, shared read-only with a group bob is in.
    club: Uuid,
}

impl World {
    fn a(&self) -> Option<(&str, &str)> {
        Some(("anna", &self.anna.1))
    }
    fn b(&self) -> Option<(&str, &str)> {
        Some(("bob", &self.bob.1))
    }
    fn c(&self) -> Option<(&str, &str)> {
        Some(("carol", &self.carol.1))
    }
}

async fn collection(pool: &PgPool, owner: Uuid, kind: CollectionKind, slug: &str, comps: &[&str]) -> Uuid {
    let new =
        NewCollection { kind, slug, display_name: slug, description: "", color: Some("#3366ccff"), components: comps };
    collections::create(pool, owner, new).await.unwrap().id
}

async fn user(pool: &PgPool, name: &str) -> (Uuid, String) {
    let id = principals::create_user(pool, name, "").await.unwrap();
    let (_, pw) = app_passwords::create(pool, id, "phone", None).await.unwrap();
    (id, pw)
}

async fn world() -> Option<World> {
    let db = Db::new().await?;
    let pool = db.pool.clone();
    let anna = user(&pool, "anna").await;
    let bob = user(&pool, "bob").await;
    let carol = user(&pool, "carol").await;
    let family = collection(&pool, anna.0, CollectionKind::Calendar, "family", &["VEVENT"]).await;
    let private = collection(&pool, anna.0, CollectionKind::Calendar, "private", &[]).await;
    collection(&pool, anna.0, CollectionKind::AddressBook, "contacts", &[]).await;
    let club = collection(&pool, carol.0, CollectionKind::Calendar, "club", &[]).await;
    collections::set_grant(&pool, anna.0, family, bob.0, Some(Access::Write)).await.unwrap();
    let members = principals::create_group(&pool, "members", "").await.unwrap();
    principals::add_member(&pool, members, bob.0).await.unwrap();
    collections::set_grant(&pool, carol.0, club, members, Some(Access::Read)).await.unwrap();
    let app = db.app();
    Some(World { db, app, anna, bob, carol, family, private, club })
}

/// A parsed multistatus: href to (properties found, status lines).
struct Ms {
    responses: Vec<(String, HashMap<String, Element>, Vec<String>)>,
    token: Option<String>,
}

impl Ms {
    fn parse(r: &Reply) -> Ms {
        assert_eq!(r.status, StatusCode::MULTI_STATUS, "{}", r.body);
        let root = xml::parse(r.body.as_bytes()).unwrap();
        let mut responses = Vec::new();
        for resp in root.children_named(DAV, "response") {
            let href = resp.child(DAV, "href").unwrap().text.clone();
            let mut found = HashMap::new();
            let mut statuses = Vec::new();
            if let Some(s) = resp.child(DAV, "status") {
                statuses.push(s.text.clone());
            }
            for ps in resp.children_named(DAV, "propstat") {
                let status = ps.child(DAV, "status").unwrap().text.clone();
                if status.contains(" 200 ") {
                    for p in &ps.child(DAV, "prop").unwrap().children {
                        found.insert(p.name.local.clone(), p.clone());
                    }
                }
                statuses.push(status);
            }
            responses.push((href, found, statuses));
        }
        Ms { responses, token: root.child(DAV, "sync-token").map(|t| t.text.clone()) }
    }

    fn get(&self, href: &str) -> &HashMap<String, Element> {
        &self.responses.iter().find(|(h, ..)| h == href).unwrap_or_else(|| panic!("no response for {href}")).1
    }

    fn hrefs(&self) -> Vec<&str> {
        self.responses.iter().map(|(h, ..)| h.as_str()).collect()
    }
}

fn hrefs_in(el: &Element) -> Vec<String> {
    el.children_named(DAV, "href").map(|h| h.text.clone()).collect()
}

fn privileges(el: &Element) -> Vec<String> {
    el.children_named(DAV, "privilege").filter_map(|p| p.children.first()).map(|c| c.name.local.clone()).collect()
}

const PROPFIND_CUP: &str =
    r#"<?xml version="1.0" encoding="utf-8"?><propfind xmlns="DAV:"><prop><current-user-principal/></prop></propfind>"#;

/// What DAVx5 asks a principal for.
const PROPFIND_PRINCIPAL: &str = r#"<propfind xmlns="DAV:" xmlns:CAL="urn:ietf:params:xml:ns:caldav" xmlns:CARD="urn:ietf:params:xml:ns:carddav">
  <prop><CAL:calendar-home-set/><CARD:addressbook-home-set/><displayname/><group-membership/><CAL:calendar-user-address-set/></prop></propfind>"#;

/// What DAVx5 asks a calendar home for (depth 1).
const PROPFIND_HOME: &str = r#"<propfind xmlns="DAV:" xmlns:CAL="urn:ietf:params:xml:ns:caldav" xmlns:IC="http://apple.com/ns/ical/" xmlns:CS="http://calendarserver.org/ns/">
  <prop><resourcetype/><displayname/><owner/><current-user-privilege-set/><IC:calendar-color/>
  <CAL:calendar-description/><CAL:calendar-timezone/><CAL:supported-calendar-component-set/><CS:source/>
  <sync-token/><CS:getctag/><supported-report-set/></prop></propfind>"#;

const SYNC_ETAGS: &str = r#"<sync-collection xmlns="DAV:"><sync-token>TOKEN</sync-token><sync-level>1</sync-level><prop><getetag/></prop></sync-collection>"#;

fn sync_body(token: &str) -> String {
    SYNC_ETAGS.replace("TOKEN", token)
}

fn multiget(hrefs: &[&str]) -> String {
    let h: String = hrefs.iter().map(|h| format!("<d:href>{h}</d:href>")).collect();
    format!(
        r#"<c:calendar-multiget xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/><c:calendar-data/></d:prop>{h}</c:calendar-multiget>"#
    )
}

#[tokio::test]
async fn door_off_mounts_nothing() {
    let Some(db) = Db::new().await else { return };
    let off = calzone_server::router(calzone_server::AppState { dav: false, ..db.state() });
    assert_eq!(send(&off, "GET", "/.well-known/caldav", None, &[], "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(send(&off, "PROPFIND", "/dav/", None, &[], "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(send(&off, "OPTIONS", "/dav/", None, &[], "").await.status, StatusCode::NOT_FOUND);
    assert_eq!(
        send(&off, "PROPFIND", "/", None, &[], "").await.status,
        StatusCode::METHOD_NOT_ALLOWED,
        "only the web app's GET"
    );
    assert_eq!(common::get(&off, "/healthz").await.0, StatusCode::OK, "the rest still works");
    let on = db.app();
    for wk in ["/.well-known/caldav", "/.well-known/carddav"] {
        let r = send(&on, "PROPFIND", wk, None, &[], "").await;
        assert_eq!(r.status, StatusCode::MOVED_PERMANENTLY);
        assert_eq!(r.header("location").as_deref(), Some("/dav/"));
    }
    let r = send(&on, "OPTIONS", "/dav/", None, &[], "").await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.header("dav").unwrap().contains("calendar-access"));
    db.drop().await;
}

#[tokio::test]
async fn auth_failures_look_alike_and_are_throttled() {
    let Some(w) = world().await else { return };
    let pool = &w.db.pool;
    let ok = send(&w.app, "PROPFIND", "/dav/", w.a(), &[("depth", "0")], PROPFIND_CUP).await;
    assert_eq!(ok.status, StatusCode::MULTI_STATUS);
    // Case, spaces and dashes in a retyped password do not matter.
    let retyped = w.anna.1.to_uppercase().replace('-', " ");
    assert_eq!(
        send(&w.app, "PROPFIND", "/dav/", Some(("Anna", &retyped)), &[], "").await.status,
        StatusCode::MULTI_STATUS
    );

    let none = send(&w.app, "PROPFIND", "/dav/", None, &[], "").await;
    assert_eq!(none.status, StatusCode::UNAUTHORIZED);
    assert!(none.header("www-authenticate").unwrap().starts_with("Basic"));
    let cases: [(&str, &str); 4] =
        [("anna", "wrong"), ("nobody", &w.anna.1), ("bob", &w.anna.1), ("members", &w.anna.1)];
    for (u, p) in cases {
        let r = send(&w.app, "PROPFIND", "/dav/", Some((u, p)), &[], "").await;
        assert_eq!((r.status, r.body.as_str()), (StatusCode::UNAUTHORIZED, ""), "{u}");
    }
    // A disabled user and a revoked password are refused the same way.
    principals::set_disabled(pool, w.carol.0, true).await.unwrap();
    assert_eq!(send(&w.app, "PROPFIND", "/dav/", w.c(), &[], "").await.status, StatusCode::UNAUTHORIZED);
    let id = app_passwords::list(pool, w.bob.0).await.unwrap()[0].id;
    app_passwords::revoke(pool, w.bob.0, id).await.unwrap();
    assert_eq!(send(&w.app, "PROPFIND", "/dav/", w.b(), &[], "").await.status, StatusCode::UNAUTHORIZED);
    assert!(app_passwords::revoke(pool, w.anna.0, id).await.is_err(), "another user's id is not found");
    assert!(app_passwords::list(pool, w.anna.0).await.unwrap()[0].last_used_at.is_some());

    // 6 failures so far; the 10th blocks the address, good password or not.
    for _ in 0..4 {
        send(&w.app, "PROPFIND", "/dav/", Some(("anna", "wrong")), &[], "").await;
    }
    let blocked = send(&w.app, "PROPFIND", "/dav/", w.a(), &[], "").await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(blocked.header("retry-after").is_some());
    w.db.drop().await;
}

#[tokio::test]
async fn davx5_discovery() {
    let Some(w) = world().await else { return };
    let r = send(&w.app, "PROPFIND", "/dav/", w.b(), &[("depth", "0")], PROPFIND_CUP).await;
    let ms = Ms::parse(&r);
    assert_eq!(hrefs_in(&ms.get("/dav/")["current-user-principal"]), ["/dav/principals/bob/"]);

    // The bare server address answers PROPFIND too, under its own href.
    let ms = Ms::parse(&send(&w.app, "PROPFIND", "/", w.b(), &[("depth", "0")], PROPFIND_CUP).await);
    assert_eq!(hrefs_in(&ms.get("/")["current-user-principal"]), ["/dav/principals/bob/"]);
    let web = send(&w.app, "GET", "/", w.b(), &[], "").await;
    assert_eq!(web.header("location").as_deref(), Some("/login"), "GET / is the web app's");
    let ms = Ms::parse(
        &send(&w.app, "PROPFIND", "/dav/principals/bob/", w.b(), &[("depth", "0")], PROPFIND_PRINCIPAL).await,
    );
    let p = ms.get("/dav/principals/bob/");
    assert_eq!(hrefs_in(&p["calendar-home-set"]), ["/dav/calendars/bob/"]);
    assert_eq!(hrefs_in(&p["addressbook-home-set"]), ["/dav/addressbooks/bob/"]);
    assert_eq!(p["displayname"].text, "bob");

    // bob's home lists what was shared with him, at the owner's path; anna's
    // private calendar is not there.
    let ms = Ms::parse(&send(&w.app, "PROPFIND", "/dav/calendars/bob/", w.b(), &[("depth", "1")], PROPFIND_HOME).await);
    assert_eq!(ms.hrefs(), ["/dav/calendars/bob/", "/dav/calendars/anna/family/", "/dav/calendars/carol/club/"]);
    let family = ms.get("/dav/calendars/anna/family/");
    assert!(family["resourcetype"].child(CALDAV, "calendar").is_some());
    assert_eq!(family["calendar-color"].text, "#3366ccff");
    assert_eq!(hrefs_in(&family["owner"]), ["/dav/principals/anna/"]);
    let privs = privileges(&family["current-user-privilege-set"]);
    assert!(privs.contains(&"write-content".into()) && !privs.contains(&"write-properties".into()), "{privs:?}");
    let comps: Vec<_> =
        family["supported-calendar-component-set"].children.iter().filter_map(|c| c.attr("name")).collect();
    assert_eq!(comps, ["VEVENT"]);
    let reports = &family["supported-report-set"];
    let names: Vec<_> = reports
        .children
        .iter()
        .flat_map(|r| &r.children)
        .flat_map(|r| &r.children)
        .map(|n| n.name.local.as_str())
        .collect();
    assert_eq!(names, ["sync-collection", "calendar-multiget", "calendar-query"]);
    assert!(family["sync-token"].text.starts_with("urn:calzone:sync:"));
    assert_eq!(family["getctag"].text, family["sync-token"].text);
    let club = privileges(&ms.get("/dav/calendars/carol/club/")["current-user-privilege-set"]);
    assert_eq!(club, ["read", "read-current-user-privilege-set"], "read-only through the group");

    let ms =
        Ms::parse(&send(&w.app, "PROPFIND", "/dav/calendars/anna/", w.a(), &[("depth", "1")], PROPFIND_HOME).await);
    assert_eq!(ms.hrefs(), ["/dav/calendars/anna/", "/dav/calendars/anna/family/", "/dav/calendars/anna/private/"]);
    let own = privileges(&ms.get("/dav/calendars/anna/family/")["current-user-privilege-set"]);
    assert!(own.contains(&"all".into()));
    let ms = Ms::parse(&send(&w.app, "PROPFIND", "/dav/addressbooks/anna/", w.a(), &[("depth", "1")], "").await);
    assert_eq!(ms.hrefs(), ["/dav/addressbooks/anna/", "/dav/addressbooks/anna/contacts/"]);
    let book = ms.get("/dav/addressbooks/anna/contacts/");
    assert!(book["resourcetype"].child(CARDDAV, "addressbook").is_some(), "allprop");
    assert_eq!(book["supported-address-data"].children[0].attr("version"), Some("3.0"));

    // Unknown properties come back as 404 propstat; depth infinity is refused.
    let r = send(
        &w.app,
        "PROPFIND",
        "/dav/calendars/anna/family/",
        w.a(),
        &[("depth", "0")],
        r#"<propfind xmlns="DAV:"><prop><displayname/><x:odd xmlns:x="urn:example"/></prop></propfind>"#,
    )
    .await;
    assert!(r.body.contains("urn:example") && r.body.contains("404 Not Found"));
    let r = send(&w.app, "PROPFIND", "/dav/calendars/anna/", w.a(), &[("depth", "infinity")], "").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.body.contains("propfind-finite-depth"));
    w.db.drop().await;
}

#[tokio::test]
async fn sync_cycle_with_conditional_writes() {
    let Some(w) = world().await else { return };
    let base = "/dav/calendars/anna/family/";
    let href = "/dav/calendars/anna/family/e1.ics";
    let body = event("e1", "20260310T090000Z", "");

    // Initial sync of an empty calendar: no responses, a token.
    let ms = Ms::parse(&send(&w.app, "REPORT", base, w.a(), &[("depth", "1")], &sync_body("")).await);
    assert!(ms.responses.is_empty());
    let t0 = ms.token.unwrap();

    let created =
        send(&w.app, "PUT", href, w.a(), &[("if-none-match", "*"), ("content-type", "text/calendar")], &body).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let etag1 = created.header("etag").unwrap();
    let again = send(&w.app, "PUT", href, w.a(), &[("if-none-match", "*")], &body).await;
    assert_eq!(again.status, StatusCode::PRECONDITION_FAILED, "create twice");

    let got = send(&w.app, "GET", href, w.a(), &[], "").await;
    assert_eq!((got.status, got.body.as_str()), (StatusCode::OK, body.as_str()));
    assert_eq!(got.header("etag"), Some(etag1.clone()));
    assert!(got.header("content-type").unwrap().starts_with("text/calendar"));

    let ms = Ms::parse(&send(&w.app, "REPORT", base, w.a(), &[], &sync_body(&t0)).await);
    assert_eq!(ms.hrefs(), [href]);
    assert_eq!(ms.get(href)["getetag"].text, etag1);
    let t1 = ms.token.unwrap();
    assert_ne!(t0, t1);

    // Update with the right ETag; a stale one is refused.
    let edited = event("e1", "20260310T100000Z", "");
    let upd = send(&w.app, "PUT", href, w.a(), &[("if-match", &etag1)], &edited).await;
    assert_eq!(upd.status, StatusCode::NO_CONTENT);
    let etag2 = upd.header("etag").unwrap();
    assert_ne!(etag1, etag2);
    let stale = send(&w.app, "PUT", href, w.b(), &[("if-match", &etag1)], &body).await;
    assert_eq!(stale.status, StatusCode::PRECONDITION_FAILED, "a stale edit is refused, never merged");
    assert_eq!(
        send(
            &w.app,
            "PUT",
            "/dav/calendars/anna/family/new.ics",
            w.a(),
            &[("if-match", "*")],
            &event("n", "20260310T100000Z", "")
        )
        .await
        .status,
        StatusCode::PRECONDITION_FAILED,
        "If-Match: * needs an existing object"
    );

    // Depth 1 PROPFIND lists objects with ETags (the CTag fallback path).
    let ms = Ms::parse(&send(&w.app, "PROPFIND", base, w.a(), &[("depth", "1")],
        r#"<propfind xmlns="DAV:" xmlns:CS="http://calendarserver.org/ns/"><prop><getetag/><CS:getctag/><getcontenttype/></prop></propfind>"#).await);
    assert_eq!(ms.hrefs(), [base, href]);
    assert_eq!(ms.get(href)["getetag"].text, etag2);
    assert_eq!(ms.get(href)["getcontenttype"].text, "text/calendar; charset=utf-8; component=vevent");

    // Multiget: present and missing hrefs, full URLs too; data round-trips
    // through XML byte for byte (CRLF, &, <).
    let full = format!("https://cal.example.com{href}");
    let ms = Ms::parse(
        &send(
            &w.app,
            "REPORT",
            base,
            w.a(),
            &[],
            &multiget(&[&full, "/dav/calendars/anna/family/missing.ics", "/dav/calendars/anna/private/e1.ics"]),
        )
        .await,
    );
    assert_eq!(ms.responses.len(), 3);
    assert_eq!(ms.get(href)["calendar-data"].text, edited);
    assert!(ms.responses[1].2[0].contains("404") && ms.responses[2].2[0].contains("404"));

    // Delete with a stale ETag fails; with the right one, a tombstone syncs.
    let del = send(&w.app, "DELETE", href, w.a(), &[("if-match", &etag1)], "").await;
    assert_eq!(del.status, StatusCode::PRECONDITION_FAILED);
    assert_eq!(send(&w.app, "DELETE", href, w.a(), &[("if-match", &etag2)], "").await.status, StatusCode::NO_CONTENT);
    assert_eq!(send(&w.app, "GET", href, w.a(), &[], "").await.status, StatusCode::NOT_FOUND);
    let ms = Ms::parse(&send(&w.app, "REPORT", base, w.b(), &[], &sync_body(&t1)).await);
    assert_eq!(ms.hrefs(), [href]);
    assert!(ms.responses[0].2[0].contains("404 Not Found"));

    // Tokens: another collection's, a future one, garbage.
    let private_token = format!("urn:calzone:sync:{}:1", w.private);
    let future = format!("urn:calzone:sync:{}:99", w.family);
    for bad in [private_token.as_str(), &future, "garbage"] {
        let r = send(&w.app, "REPORT", base, w.a(), &[], &sync_body(bad)).await;
        assert_eq!(r.status, StatusCode::FORBIDDEN, "{bad}");
        assert!(r.body.contains("valid-sync-token"));
    }
    w.db.drop().await;
}

#[tokio::test]
async fn calendar_query_time_range() {
    let Some(w) = world().await else { return };
    let base = "/dav/calendars/anna/private/";
    let weekly = event("w", "20260105T090000Z", "RRULE:FREQ=WEEKLY;COUNT=4\r\n");
    for (name, body) in [("w.ics", weekly), ("t.ics", todo("t")), ("late.ics", event("late", "20270101T090000Z", ""))] {
        assert_eq!(send(&w.app, "PUT", &format!("{base}{name}"), w.a(), &[], &body).await.status, StatusCode::CREATED);
    }
    let query = |comp: &str, range: &str| {
        format!(
            r#"<c:calendar-query xmlns:d="DAV:" xmlns:c="urn:ietf:params:xml:ns:caldav"><d:prop><d:getetag/></d:prop>
               <c:filter><c:comp-filter name="VCALENDAR"><c:comp-filter name="{comp}">{range}</c:comp-filter></c:comp-filter></c:filter></c:calendar-query>"#
        )
    };
    let names =
        |r: &Reply| Ms::parse(r).hrefs().iter().map(|h| h.rsplit('/').next().unwrap().to_string()).collect::<Vec<_>>();
    let all = send(&w.app, "REPORT", base, w.a(), &[("depth", "1")], &query("VEVENT", "")).await;
    assert_eq!(names(&all), ["late.ics", "w.ics"]);
    let week2 = query("VEVENT", r#"<c:time-range start="20260112T000000Z" end="20260113T000000Z"/>"#);
    assert_eq!(names(&send(&w.app, "REPORT", base, w.a(), &[], &week2).await), ["w.ics"]);
    let gap = query("VEVENT", r#"<c:time-range start="20260107T000000Z" end="20260111T000000Z"/>"#);
    assert!(names(&send(&w.app, "REPORT", base, w.a(), &[], &gap).await).is_empty(), "between instances");
    let open = query("VEVENT", r#"<c:time-range start="20260601T000000Z"/>"#);
    assert_eq!(names(&send(&w.app, "REPORT", base, w.a(), &[], &open).await), ["late.ics"]);
    assert_eq!(names(&send(&w.app, "REPORT", base, w.a(), &[], &query("VTODO", "")).await), ["t.ics"]);
    let alarm = query("VEVENT", r#"<c:comp-filter name="VALARM"/>"#);
    let r = send(&w.app, "REPORT", base, w.a(), &[], &alarm).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.body.contains("supported-filter"));
    let r = send(&w.app, "REPORT", base, w.a(), &[], r#"<D:expand-property xmlns:D="DAV:"/>"#).await;
    assert!(r.status == StatusCode::FORBIDDEN && r.body.contains("supported-report"));
    w.db.drop().await;
}

#[tokio::test]
async fn alarms_stay_personal_over_dav() {
    let Some(w) = world().await else { return };
    let href = "/dav/calendars/anna/family/shared.ics";
    let with_annas = event("shared", "20260401T090000Z", ALARM);
    assert_eq!(send(&w.app, "PUT", href, w.a(), &[], &with_annas).await.status, StatusCode::CREATED);
    let bobs_view = send(&w.app, "GET", href, w.b(), &[], "").await;
    assert!(!bobs_view.body.contains("VALARM"), "bob never sees anna's alarm");
    let bobs_alarm = ALARM.replace("-PT15M", "-PT1H");
    let mine = event("shared", "20260401T090000Z", &bobs_alarm);
    let etag = bobs_view.header("etag").unwrap();
    assert_eq!(send(&w.app, "PUT", href, w.b(), &[("if-match", &etag)], &mine).await.status, StatusCode::NO_CONTENT);
    let annas = send(&w.app, "GET", href, w.a(), &[], "").await.body;
    assert!(annas.contains("-PT15M") && !annas.contains("-PT1H"), "anna keeps hers only");
    let ms = Ms::parse(&send(&w.app, "REPORT", "/dav/calendars/anna/family/", w.b(), &[], &multiget(&[href])).await);
    let bobs = &ms.get(href)["calendar-data"].text;
    assert!(bobs.contains("-PT1H") && !bobs.contains("-PT15M"), "multiget serves bob's own layer");
    w.db.drop().await;
}

#[tokio::test]
async fn sharing_and_strangers() {
    let Some(w) = world().await else { return };
    let ev = event("c1", "20260501T090000Z", "");
    assert_eq!(
        send(&w.app, "PUT", "/dav/calendars/carol/club/c1.ics", w.c(), &[], &ev).await.status,
        StatusCode::CREATED
    );
    // bob reads club through the group, but cannot write or delete.
    assert_eq!(send(&w.app, "GET", "/dav/calendars/carol/club/c1.ics", w.b(), &[], "").await.status, StatusCode::OK);
    let put =
        send(&w.app, "PUT", "/dav/calendars/carol/club/c2.ics", w.b(), &[], &event("c2", "20260501T090000Z", "")).await;
    assert_eq!(put.status, StatusCode::FORBIDDEN);
    assert!(put.body.contains("need-privileges"));
    assert_eq!(
        send(&w.app, "DELETE", "/dav/calendars/carol/club/c1.ics", w.b(), &[], "").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&w.app, "DELETE", "/dav/calendars/carol/club/zz.ics", w.b(), &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    // anna sees nothing of carol's: every path is 404, as for one that does not exist.
    for (m, p) in [
        ("PROPFIND", "/dav/calendars/carol/club/"),
        ("GET", "/dav/calendars/carol/club/c1.ics"),
        ("PROPFIND", "/dav/calendars/carol/"),
        ("PROPFIND", "/dav/principals/carol/"),
        ("PROPFIND", "/dav/calendars/carol/nope/"),
        ("PUT", "/dav/calendars/carol/club/x.ics"),
        ("DELETE", "/dav/calendars/carol/club/"),
    ] {
        assert_eq!(send(&w.app, m, p, w.a(), &[("depth", "0")], "").await.status, StatusCode::NOT_FOUND, "{m} {p}");
    }
    // A calendar path under the address book tree is not found either.
    assert_eq!(
        send(&w.app, "PROPFIND", "/dav/addressbooks/anna/family/", w.a(), &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    let _ = w.club;
    w.db.drop().await;
}

#[tokio::test]
async fn scoped_app_password() {
    let Some(w) = world().await else { return };
    let pool = &w.db.pool;
    let scope: Scope = [(w.family, Access::Read)].into();
    let (_, pw) = app_passwords::create(pool, w.anna.0, "tablet", Some(&scope)).await.unwrap();
    let t = Some(("anna", pw.as_str()));
    let ms = Ms::parse(&send(&w.app, "PROPFIND", "/dav/calendars/anna/", t, &[("depth", "1")], PROPFIND_HOME).await);
    assert_eq!(ms.hrefs(), ["/dav/calendars/anna/", "/dav/calendars/anna/family/"]);
    let privs = privileges(&ms.get("/dav/calendars/anna/family/")["current-user-privilege-set"]);
    assert_eq!(privs, ["read", "read-current-user-privilege-set"], "capped by the scope, though anna owns it");
    let put =
        send(&w.app, "PUT", "/dav/calendars/anna/family/x.ics", t, &[], &event("x", "20260501T090000Z", "")).await;
    assert_eq!(put.status, StatusCode::FORBIDDEN);
    assert_eq!(
        send(&w.app, "PROPFIND", "/dav/calendars/anna/private/", t, &[], "").await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(send(&w.app, "MKCALENDAR", "/dav/calendars/anna/new/", t, &[], "").await.status, StatusCode::FORBIDDEN);
    assert!(
        Ms::parse(&send(&w.app, "PROPFIND", "/dav/addressbooks/anna/", t, &[("depth", "1")], "").await).responses.len()
            == 1
    );
    let listed = app_passwords::list(pool, w.anna.0).await.unwrap();
    assert_eq!(listed[0].scope.as_ref().unwrap().get(&w.family), Some(&Access::Read));
    assert!(
        app_passwords::create(pool, w.anna.0, "x", Some(&[(w.club, Access::Read)].into())).await.is_err(),
        "only what anna can see"
    );
    w.db.drop().await;
}

#[tokio::test]
async fn collections_over_dav() {
    let Some(w) = world().await else { return };
    // DAVx5's MKCALENDAR, calendar-timezone included.
    let mk = r##"<?xml version="1.0" encoding="UTF-8"?><CAL:mkcalendar xmlns="DAV:" xmlns:CAL="urn:ietf:params:xml:ns:caldav" xmlns:ICAL="http://apple.com/ns/ical/">
      <set><prop><displayname>Work</displayname><ICAL:calendar-color>#ff8800ff</ICAL:calendar-color>
      <CAL:calendar-timezone>BEGIN:VCALENDAR&#13;
END:VCALENDAR</CAL:calendar-timezone>
      <CAL:supported-calendar-component-set><CAL:comp name="VEVENT"/><CAL:comp name="VTODO"/></CAL:supported-calendar-component-set>
      </prop></set></CAL:mkcalendar>"##;
    let uuid_slug = "/dav/calendars/anna/0f3a6c1e-2b4d-4e8f-9a7b-1c2d3e4f5a6b/";
    assert_eq!(send(&w.app, "MKCALENDAR", uuid_slug, w.a(), &[], mk).await.status, StatusCode::CREATED);
    assert_eq!(send(&w.app, "MKCALENDAR", uuid_slug, w.a(), &[], mk).await.status, StatusCode::METHOD_NOT_ALLOWED);
    let ms = Ms::parse(&send(&w.app, "PROPFIND", uuid_slug, w.a(), &[("depth", "0")], PROPFIND_HOME).await);
    let p = ms.get(uuid_slug);
    assert_eq!((p["displayname"].text.as_str(), p["calendar-color"].text.as_str()), ("Work", "#ff8800ff"));
    assert_eq!(p["supported-calendar-component-set"].children.len(), 2);

    let patch = |body: &'static str| send(&w.app, "PROPPATCH", uuid_slug, w.a(), &[], body);
    let r = patch(r##"<d:propertyupdate xmlns:d="DAV:" xmlns:i="http://apple.com/ns/ical/"><d:set><d:prop>
        <d:displayname>Office</d:displayname><i:calendar-color>#112233</i:calendar-color></d:prop></d:set></d:propertyupdate>"##).await;
    assert_eq!(Ms::parse(&r).responses[0].2, ["HTTP/1.1 200 OK"]);
    let r = patch(
        r##"<d:propertyupdate xmlns:d="DAV:" xmlns:x="urn:example"><d:set><d:prop>
        <d:displayname>Never</d:displayname><x:odd>1</x:odd></d:prop></d:set></d:propertyupdate>"##,
    )
    .await;
    let statuses = &Ms::parse(&r).responses[0].2;
    assert!(statuses.iter().any(|s| s.contains("403")) && statuses.iter().any(|s| s.contains("424")));
    let ms = Ms::parse(&send(&w.app, "PROPFIND", uuid_slug, w.a(), &[("depth", "0")], PROPFIND_HOME).await);
    assert_eq!(ms.get(uuid_slug)["displayname"].text, "Office", "all or nothing");
    assert_eq!(ms.get(uuid_slug)["calendar-color"].text, "#112233");
    // bob may write family's events but not rename it.
    let r = send(&w.app, "PROPPATCH", "/dav/calendars/anna/family/", w.b(), &[],
        r#"<d:propertyupdate xmlns:d="DAV:"><d:set><d:prop><d:displayname>Mine</d:displayname></d:prop></d:set></d:propertyupdate>"#).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // Extended MKCOL for an address book; plain collections are refused.
    let mkcol = r#"<D:mkcol xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav"><D:set><D:prop>
        <D:resourcetype><D:collection/><C:addressbook/></D:resourcetype><D:displayname>Work contacts</D:displayname>
        <C:addressbook-description>From the office</C:addressbook-description></D:prop></D:set></D:mkcol>"#;
    assert_eq!(
        send(&w.app, "MKCOL", "/dav/addressbooks/anna/work/", w.a(), &[], mkcol).await.status,
        StatusCode::CREATED
    );
    assert_eq!(
        send(&w.app, "MKCOL", "/dav/addressbooks/anna/plain/", w.a(), &[], "").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&w.app, "MKCOL", "/dav/calendars/anna/wrongtree/", w.a(), &[], mkcol).await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        send(&w.app, "MKCALENDAR", "/dav/calendars/bob/x/", w.a(), &[], "").await.status,
        StatusCode::FORBIDDEN,
        "only in your own home"
    );
    assert_eq!(
        send(&w.app, "MKCALENDAR", "/dav/calendars/anna/Bad%20Name/", w.a(), &[], "").await.status,
        StatusCode::FORBIDDEN
    );

    // Deleting: bob (write) may not delete family; anna deletes her new one.
    assert_eq!(
        send(&w.app, "DELETE", "/dav/calendars/anna/family/", w.b(), &[], "").await.status,
        StatusCode::FORBIDDEN
    );
    assert_eq!(send(&w.app, "DELETE", uuid_slug, w.a(), &[], "").await.status, StatusCode::NO_CONTENT);
    assert_eq!(send(&w.app, "PROPFIND", uuid_slug, w.a(), &[], "").await.status, StatusCode::NOT_FOUND);
    w.db.drop().await;
}

#[tokio::test]
async fn write_validation() {
    let Some(w) = world().await else { return };
    let base = "/dav/calendars/anna/family/";
    assert_eq!(
        send(&w.app, "PUT", &format!("{base}a.ics"), w.a(), &[], &event("same", "20260501T090000Z", "")).await.status,
        StatusCode::CREATED
    );
    let clash = send(&w.app, "PUT", &format!("{base}b.ics"), w.a(), &[], &event("same", "20260502T090000Z", "")).await;
    assert_eq!(clash.status, StatusCode::FORBIDDEN);
    assert!(
        clash.body.contains("no-uid-conflict") && clash.body.contains("/dav/calendars/anna/family/a.ics"),
        "{}",
        clash.body
    );
    let bad = send(&w.app, "PUT", &format!("{base}c.ics"), w.a(), &[], "BEGIN:VCALENDAR\r\nnonsense").await;
    assert!(bad.status == StatusCode::FORBIDDEN && bad.body.contains("valid-calendar-data"));
    let vcard = send(&w.app, "PUT", &format!("{base}d.ics"), w.a(), &[], &card("x", "X")).await;
    assert!(vcard.status == StatusCode::FORBIDDEN && vcard.body.contains("supported-calendar-data"));
    let task = send(&w.app, "PUT", &format!("{base}e.ics"), w.a(), &[], &todo("t")).await;
    assert!(
        task.status == StatusCode::FORBIDDEN && task.body.contains("supported-calendar-component"),
        "family takes events only"
    );
    let ev =
        send(&w.app, "PUT", "/dav/addressbooks/anna/contacts/e.vcf", w.a(), &[], &event("e", "20260501T090000Z", ""))
            .await;
    assert!(ev.status == StatusCode::FORBIDDEN && ev.body.contains("supported-address-data"));
    assert_eq!(send(&w.app, "GET", base, w.a(), &[], "").await.status, StatusCode::METHOD_NOT_ALLOWED);
    assert_eq!(send(&w.app, "LOCK", base, w.a(), &[], "").await.status, StatusCode::METHOD_NOT_ALLOWED);
    w.db.drop().await;
}

#[tokio::test]
async fn carddav_cycle() {
    let Some(w) = world().await else { return };
    let base = "/dav/addressbooks/anna/contacts/";
    let photo = include_str!("../../core/tests/fixtures/vcard3-photo.vcf");
    let cards = [
        ("ada.vcf", card("ada", "Ada Lovelace")),
        ("grace.vcf", card("grace", "Grace Hopper")),
        ("photo.vcf", photo.to_string()),
    ];
    for (name, body) in &cards {
        let r = send(
            &w.app,
            "PUT",
            &format!("{base}{name}"),
            w.a(),
            &[("if-none-match", "*"), ("content-type", "text/vcard")],
            body,
        )
        .await;
        assert_eq!(r.status, StatusCode::CREATED, "{name}");
    }
    let ms = Ms::parse(&send(&w.app, "REPORT", base, w.a(), &[], &sync_body("")).await);
    assert_eq!(ms.responses.len(), 3);
    let hrefs: Vec<String> = cards.iter().map(|(n, _)| format!("{base}{n}")).collect();
    let body: String = hrefs.iter().map(|h| format!("<d:href>{h}</d:href>")).collect();
    let mg = format!(
        r#"<card:addressbook-multiget xmlns:d="DAV:" xmlns:card="urn:ietf:params:xml:ns:carddav"><d:prop><d:getetag/><card:address-data/></d:prop>{body}</card:addressbook-multiget>"#
    );
    let ms = Ms::parse(&send(&w.app, "REPORT", base, w.a(), &[("depth", "1")], &mg).await);
    for ((_, original), href) in cards.iter().zip(&hrefs) {
        assert_eq!(&ms.get(href)["address-data"].text, original, "{href} byte for byte");
    }
    let q = r#"<C:addressbook-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:carddav"><D:prop><D:getetag/></D:prop>
        <C:filter><C:prop-filter name="FN"><C:text-match collation="i;unicode-casemap" match-type="contains">hopper</C:text-match></C:prop-filter></C:filter></C:addressbook-query>"#;
    let ms = Ms::parse(&send(&w.app, "REPORT", base, w.a(), &[], q).await);
    assert_eq!(ms.hrefs(), [format!("{base}grace.vcf")]);
    // A calendar REPORT on an address book is not supported.
    let r = send(&w.app, "REPORT", base, w.a(), &[], &multiget(&[&hrefs[0]])).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    w.db.drop().await;
}

#[tokio::test]
async fn thunderbird_calendar_flow() {
    let Some(w) = world().await else { return };
    let base = "/dav/calendars/anna/private/";
    // Thunderbird's first look at a calendar URL.
    let r = send(&w.app, "PROPFIND", base, w.a(), &[("depth", "0")], r#"<D:propfind xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/" xmlns:C="urn:ietf:params:xml:ns:caldav">
        <D:prop><D:resourcetype/><D:owner/><D:current-user-principal/><D:current-user-privilege-set/><D:supported-report-set/>
        <C:supported-calendar-component-set/><CS:getctag/></D:prop></D:propfind>"#).await;
    let ms = Ms::parse(&r);
    let p = ms.get(base);
    let ctag0 = p["getctag"].text.clone();
    assert_eq!(p["supported-calendar-component-set"].children.len(), 3, "no restriction lists all three");
    // Thunderbird writes with a uuid name and If-None-Match, then edits with If-Match.
    let href = format!("{base}8c3e5f1a-1111-4222-8333-944455556666.ics");
    let created = send(&w.app, "PUT", &href, w.a(), &[("if-none-match", "*")], &todo("tb-task")).await;
    assert_eq!(created.status, StatusCode::CREATED);
    let r = send(&w.app, "PROPFIND", base, w.a(), &[("depth", "0")], r#"<D:propfind xmlns:D="DAV:" xmlns:CS="http://calendarserver.org/ns/"><D:prop><CS:getctag/></D:prop></D:propfind>"#).await;
    assert_ne!(Ms::parse(&r).get(base)["getctag"].text, ctag0, "the CTag moves on every write");
    let done = todo("tb-task").replace("SUMMARY:Task", "SUMMARY:Task\r\nSTATUS:COMPLETED");
    let etag = created.header("etag").unwrap();
    assert_eq!(send(&w.app, "PUT", &href, w.a(), &[("if-match", &etag)], &done).await.status, StatusCode::NO_CONTENT);
    let q = r#"<C:calendar-query xmlns:D="DAV:" xmlns:C="urn:ietf:params:xml:ns:caldav"><D:prop><D:getetag/><C:calendar-data/></D:prop>
        <C:filter><C:comp-filter name="VCALENDAR"><C:comp-filter name="VTODO"><C:prop-filter name="COMPLETED"><C:is-not-defined/></C:prop-filter></C:comp-filter></C:comp-filter></C:filter></C:calendar-query>"#;
    assert_eq!(
        Ms::parse(&send(&w.app, "REPORT", base, w.a(), &[], q).await).responses.len(),
        1,
        "no COMPLETED property yet"
    );
    let _ = (CS, ICAL);
    w.db.drop().await;
}
