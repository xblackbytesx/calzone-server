//! The admin pages end to end against a real Postgres: login with uniform
//! failures and throttling, sessions, the security headers and Origin
//! check, collections (create, edit, share, typed delete), device passwords
//! that work on the DAV door, people and groups, and passphrase changes.

mod common;

use axum::Router;
use axum::http::StatusCode;
use calzone_server::domain::{accounts, principals};
use common::{Db, Reply, send};
use uuid::Uuid;

const FORM: (&str, &str) = ("content-type", "application/x-www-form-urlencoded");

fn enc(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn form(fields: &[(&str, &str)]) -> String {
    fields.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

/// A browser: one session cookie.
struct Browser {
    app: Router,
    cookie: Option<String>,
}

impl Browser {
    fn new(app: &Router) -> Self {
        Browser { app: app.clone(), cookie: None }
    }

    fn take_cookie(&mut self, r: &Reply) {
        if let Some(c) = r.header("set-cookie") {
            let v = c.split(';').next().unwrap().split_once('=').unwrap().1.to_string();
            self.cookie = if v.is_empty() { None } else { Some(v) };
        }
    }

    async fn get(&mut self, path: &str) -> Reply {
        let c = self.cookie.clone().map(|c| format!("calzone_session={c}"));
        let mut h = vec![];
        if let Some(c) = &c {
            h.push(("cookie", c.as_str()));
        }
        send(&self.app, "GET", path, None, &h, "").await
    }

    async fn post(&mut self, path: &str, fields: &[(&str, &str)]) -> Reply {
        let c = self.cookie.clone().map(|c| format!("calzone_session={c}"));
        let mut h = vec![FORM];
        if let Some(c) = &c {
            h.push(("cookie", c.as_str()));
        }
        let r = send(&self.app, "POST", path, None, &h, &form(fields)).await;
        self.take_cookie(&r);
        r
    }

    async fn login(&mut self, name: &str, pass: &str) -> Reply {
        self.post("/login", &[("name", name), ("passphrase", pass)]).await
    }
}

fn code_in(body: &str, id: &str) -> String {
    let start = format!("<code id=\"{id}\">");
    let i = body.find(&start).unwrap_or_else(|| panic!("no #{id} in page")) + start.len();
    body[i..i + body[i..].find("</code>").unwrap()].to_string()
}

fn location(r: &Reply) -> String {
    r.header("location").unwrap_or_default()
}

/// anna (admin) and bob, both with known passphrases.
async fn setup(db: &Db) -> (Uuid, Uuid) {
    let p = &db.pool;
    let anna = principals::create_user(p, "anna", "Anna").await.unwrap();
    accounts::set_admin(p, anna, true).await.unwrap();
    accounts::set_passphrase(p, anna, "anna passphrase").await.unwrap();
    let bob = principals::create_user(p, "bob", "").await.unwrap();
    accounts::set_passphrase(p, bob, "bob passphrase").await.unwrap();
    (anna, bob)
}

#[tokio::test]
async fn login_sessions_and_uniform_failures() {
    let Some(db) = Db::new().await else { return };
    let (_, _) = setup(&db).await;
    let app = db.app();
    let mut b = Browser::new(&app);
    assert_eq!(location(&b.get("/").await), "/login");
    assert_eq!(location(&b.get("/collections").await), "/login");
    let htmx = send(&app, "GET", "/collections", None, &[("hx-request", "true")], "").await;
    assert_eq!(htmx.header("hx-redirect").as_deref(), Some("/login"), "HTMX loads the login page whole");
    let page = b.get("/login").await;
    assert_eq!(page.status, StatusCode::OK);
    let csp = page.header("content-security-policy").unwrap();
    assert!(csp.contains("default-src 'none'") && csp.contains("script-src 'self'"));
    assert_eq!(page.header("x-content-type-options").as_deref(), Some("nosniff"));

    // Every failure reads the same.
    let group = principals::create_group(&db.pool, "family", "").await.unwrap();
    let _ = group;
    let nopass = principals::create_user(&db.pool, "nopass", "").await.unwrap();
    let _ = nopass;
    let off = principals::create_user(&db.pool, "carol", "").await.unwrap();
    accounts::set_passphrase(&db.pool, off, "carol passphrase").await.unwrap();
    principals::set_disabled(&db.pool, off, true).await.unwrap();
    let mut bodies = Vec::new();
    for (n, p) in [
        ("anna", "wrong one"),
        ("nobody", "anna passphrase"),
        ("family", "x"),
        ("nopass", "x"),
        ("carol", "carol passphrase"),
    ] {
        let r = Browser::new(&app).login(n, p).await;
        assert_eq!(r.status, StatusCode::UNAUTHORIZED, "{n}");
        assert!(r.header("set-cookie").is_none());
        bodies.push(r.body.replace(n, "NAME"));
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]), "the same page for every kind of failure");
    assert!(bodies[0].contains("Wrong name or passphrase."));

    let r = b.login("Anna", "anna passphrase").await;
    assert_eq!((r.status, location(&r).as_str()), (StatusCode::SEE_OTHER, "/calendar"));
    let cookie = r.header("set-cookie").unwrap();
    assert!(cookie.contains("HttpOnly") && cookie.contains("SameSite=Strict") && cookie.contains("Path=/"));
    assert!(!cookie.contains("Secure"), "the test state has CALZONE_COOKIE_SECURE off");
    assert_eq!(b.get("/collections").await.status, StatusCode::OK);
    assert_eq!(location(&b.get("/").await), "/calendar");

    // Logout ends the session for real, not just the cookie.
    let token = b.cookie.clone().unwrap();
    b.post("/logout", &[]).await;
    assert!(b.cookie.is_none());
    b.cookie = Some(token);
    assert_eq!(location(&b.get("/collections").await), "/login", "the old cookie is dead");
    db.drop().await;
}

#[tokio::test]
async fn login_throttle_and_origin_check() {
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = db.app();
    for _ in 0..5 {
        assert_eq!(Browser::new(&app).login("bob", "wrong guess").await.status, StatusCode::UNAUTHORIZED);
    }
    let r = Browser::new(&app).login("bob", "bob passphrase").await;
    assert_eq!(r.status, StatusCode::TOO_MANY_REQUESTS, "the account is locked for the window");
    assert!(r.body.contains("Too many failed attempts"));
    // Another account from the same (unknown) address still works: 5 < 10.
    let mut a = Browser::new(&app);
    assert_eq!(a.login("anna", "anna passphrase").await.status, StatusCode::SEE_OTHER);

    // A cross-site form post is refused before it reaches the page.
    let cookie = format!("calzone_session={}", a.cookie.clone().unwrap());
    let body = form(&[("kind", "calendar"), ("name", "Evil")]);
    let evil = send(
        &app,
        "POST",
        "/collections",
        None,
        &[FORM, ("cookie", &cookie), ("origin", "https://evil.example"), ("host", "cal.example")],
        &body,
    )
    .await;
    assert_eq!(evil.status, StatusCode::FORBIDDEN);
    let ours = send(
        &app,
        "POST",
        "/collections",
        None,
        &[FORM, ("cookie", &cookie), ("origin", "https://cal.example"), ("host", "cal.example")],
        &body,
    )
    .await;
    assert_eq!(ours.status, StatusCode::SEE_OTHER);
    db.drop().await;
}

#[tokio::test]
async fn collections_create_edit_share_delete() {
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = db.app();
    let mut anna = Browser::new(&app);
    anna.login("anna", "anna passphrase").await;
    let mut bob = Browser::new(&app);
    bob.login("bob", "bob passphrase").await;

    let r = anna.post("/collections", &[("kind", "calendar"), ("name", "Family <3"), ("color", "#FF8800")]).await;
    assert_eq!(r.status, StatusCode::SEE_OTHER);
    let fam = location(&r);
    let again = anna.post("/collections", &[("kind", "calendar"), ("name", "Family <3")]).await;
    assert_ne!(location(&again), fam, "a second one with the same name gets its own address");
    let page = anna.get(&fam).await.body;
    assert!(page.contains("Family &#60;3") && !page.contains("Family <3"), "names are escaped");
    assert!(page.contains("fill=\"#ff8800\""));
    assert!(page.contains("/dav/calendars/anna/family-3/"), "the DAV address uses the slug");
    let bad = anna.post("/collections", &[("kind", "calendar"), ("name", "  ")]).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad.header("hx-retarget").as_deref(), None, "a plain browser gets a whole error page");
    assert!(bad.body.contains("<!doctype html>") && bad.body.contains("notice error"));

    // bob sees nothing until it is shared; then he can edit events, not rename.
    assert_eq!(bob.get(&fam).await.status, StatusCode::NOT_FOUND);
    let bob_id = principals::by_name(&db.pool, "bob").await.unwrap().id.to_string();
    assert_eq!(
        anna.post(&format!("{fam}/share"), &[("principal", &bob_id), ("level", "write")]).await.status,
        StatusCode::SEE_OTHER
    );
    let list = bob.get("/collections").await.body;
    assert!(list.contains("Family &#60;3") && list.contains("Shared by anna · can edit"));
    let rename = bob.post(&fam, &[("name", "Mine")]).await;
    assert_eq!(rename.status, StatusCode::FORBIDDEN);
    assert!(!bob.get(&fam).await.body.contains("Type <strong>"), "no delete panel for bob");

    anna.post(&fam, &[("name", "Family"), ("description", "Ours"), ("color", "#112233")]).await;
    let page = anna.get(&fam).await.body;
    assert!(page.contains("value=\"Family\"") && page.contains("Ours") && page.contains("#112233"));

    assert_eq!(anna.post(&format!("{fam}/unshare/{bob_id}"), &[]).await.status, StatusCode::SEE_OTHER);
    assert_eq!(bob.get(&fam).await.status, StatusCode::NOT_FOUND);

    let wrong = anna.post(&format!("{fam}/delete"), &[("confirm", "Famly")]).await;
    assert_eq!(wrong.status, StatusCode::BAD_REQUEST);
    assert_eq!(anna.get(&fam).await.status, StatusCode::OK, "still there");
    let gone = anna.post(&format!("{fam}/delete"), &[("confirm", " family ")]).await;
    assert_eq!(location(&gone), "/collections");
    assert_eq!(anna.get(&fam).await.status, StatusCode::NOT_FOUND);

    let book = anna.post("/collections", &[("kind", "addressbook"), ("name", "Contacts"), ("color", "#ff0000")]).await;
    assert!(anna.get(&location(&book)).await.body.contains("/dav/addressbooks/anna/contacts/"));
    db.drop().await;
}

#[tokio::test]
async fn device_passwords_work_on_the_door() {
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = db.app();
    let mut anna = Browser::new(&app);
    anna.login("anna", "anna passphrase").await;
    let fam = location(&anna.post("/collections", &[("kind", "calendar"), ("name", "Family")]).await);
    anna.post("/collections", &[("kind", "calendar"), ("name", "Work")]).await;
    let fam_id = fam.rsplit('/').next().unwrap().to_string();

    let r = anna.post("/devices", &[("label", "Pixel <DAVx5>"), ("limit", "all")]).await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Pixel &#60;DAVx5&#62; is ready"));
    let pw = code_in(&r.body, "password");
    assert_eq!(code_in(&r.body, "user"), "anna");
    let propfind = |pw: String| {
        let app = app.clone();
        async move { send(&app, "PROPFIND", "/dav/calendars/anna/", Some(("anna", &pw)), &[("depth", "1")], "").await }
    };
    let all = propfind(pw.clone()).await;
    assert!(all.body.contains("/family/") && all.body.contains("/work/"));

    let scoped =
        anna.post("/devices", &[("label", "Tablet"), ("limit", "some"), (&format!("c-{fam_id}"), "read")]).await;
    let spw = code_in(&scoped.body, "password");
    let only = propfind(spw).await;
    assert!(only.body.contains("/family/") && !only.body.contains("/work/"));
    let none = anna.post("/devices", &[("label", "Empty"), ("limit", "some")]).await;
    assert_eq!(none.status, StatusCode::BAD_REQUEST);

    let page = anna.get("/devices").await.body;
    assert!(page.contains("Everything you can reach") && page.contains("Family (calendar) (view)"));
    let id =
        calzone_server::domain::app_passwords::list(&db.pool, principals::by_name(&db.pool, "anna").await.unwrap().id)
            .await
            .unwrap()
            .into_iter()
            .find(|p| p.label.starts_with("Pixel"))
            .unwrap()
            .id;
    assert_eq!(anna.post(&format!("/devices/{id}/revoke"), &[]).await.status, StatusCode::SEE_OTHER);
    assert_eq!(propfind(pw).await.status, StatusCode::UNAUTHORIZED, "revoked on the spot");
    let mut bob = Browser::new(&app);
    bob.login("bob", "bob passphrase").await;
    assert_eq!(bob.post(&format!("/devices/{id}/revoke"), &[]).await.status, StatusCode::NOT_FOUND);

    // Door off: Devices stays (phones with the app sign in there), without
    // the app passwords.
    let off = calzone_server::router(calzone_server::AppState { dav: false, ..db.state() });
    let mut a2 = Browser::new(&off);
    a2.login("anna", "anna passphrase").await;
    assert!(a2.get("/collections").await.body.contains("href=\"/devices\""));
    let page = a2.get("/devices").await.body;
    assert!(page.contains("Show a sign-in code") && !page.contains("Create password"));
    db.drop().await;
}

#[tokio::test]
async fn people_and_groups() {
    let Some(db) = Db::new().await else { return };
    let (anna_id, bob_id) = setup(&db).await;
    let app = db.app();
    let mut anna = Browser::new(&app);
    anna.login("anna", "anna passphrase").await;
    let mut bob = Browser::new(&app);
    bob.login("bob", "bob passphrase").await;
    assert_eq!(bob.get("/admin/people").await.status, StatusCode::FORBIDDEN);
    assert!(!bob.get("/collections").await.body.contains("/admin/people"));
    assert_eq!(bob.post("/admin/people", &[("name", "eve")]).await.status, StatusCode::FORBIDDEN);

    let r = anna.post("/admin/people", &[("name", "dana"), ("display_name", "Dana")]).await;
    assert_eq!(r.status, StatusCode::OK);
    let temp = code_in(&r.body, "passphrase");
    let mut dana = Browser::new(&app);
    assert_eq!(dana.login("dana", &temp).await.status, StatusCode::SEE_OTHER, "the temporary passphrase works");
    assert_eq!(anna.post("/admin/people", &[("name", "dana")]).await.status, StatusCode::CONFLICT);
    assert_eq!(anna.post("/admin/people", &[("name", "Bad Name")]).await.status, StatusCode::BAD_REQUEST);

    // A new passphrase ends the old one and every session.
    let dana_id = principals::by_name(&db.pool, "dana").await.unwrap().id;
    let r = anna.post(&format!("/admin/people/{dana_id}/passphrase"), &[]).await;
    let temp2 = code_in(&r.body, "passphrase");
    assert_eq!(location(&dana.get("/collections").await), "/login");
    assert_eq!(Browser::new(&app).login("dana", &temp).await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(dana.login("dana", &temp2).await.status, StatusCode::SEE_OTHER);

    // Disable signs out; your own account is out of reach.
    anna.post(&format!("/admin/people/{dana_id}/disable"), &[]).await;
    assert_eq!(location(&dana.get("/collections").await), "/login");
    assert_eq!(anna.post(&format!("/admin/people/{anna_id}/disable"), &[]).await.status, StatusCode::BAD_REQUEST);
    assert_eq!(
        anna.post(&format!("/admin/people/{anna_id}/admin"), &[("admin", "off")]).await.status,
        StatusCode::BAD_REQUEST
    );
    anna.post(&format!("/admin/people/{dana_id}/enable"), &[]).await;
    assert_eq!(Browser::new(&app).login("dana", &temp2).await.status, StatusCode::SEE_OTHER);

    anna.post(&format!("/admin/people/{bob_id}/admin"), &[("admin", "on")]).await;
    assert_eq!(bob.get("/admin/people").await.status, StatusCode::OK, "bob is an admin now");

    // Groups: share with a group, and members get it.
    anna.post("/admin/groups", &[("name", "family"), ("display_name", "Family")]).await;
    let fam_group = principals::by_name(&db.pool, "family").await.unwrap().id;
    let dana_s = dana_id.to_string();
    anna.post(&format!("/admin/groups/{fam_group}/members"), &[("member", &dana_s)]).await;
    let cal = location(&anna.post("/collections", &[("kind", "calendar"), ("name", "Home")]).await);
    let g = fam_group.to_string();
    anna.post(&format!("{cal}/share"), &[("principal", &g), ("level", "read")]).await;
    let mut dana = Browser::new(&app);
    dana.login("dana", &temp2).await;
    assert!(dana.get("/collections").await.body.contains("Shared by anna · can view"));
    let anna_s = anna_id.to_string();
    assert_eq!(
        anna.post(&format!("/admin/groups/{anna_s}/members"), &[("member", &dana_s)]).await.status,
        StatusCode::BAD_REQUEST,
        "not a group"
    );
    anna.post(&format!("/admin/groups/{fam_group}/members/{dana_id}/remove"), &[]).await;
    assert!(!dana.get("/collections").await.body.contains("Home"));
    let people = anna.get("/admin/people").await.body;
    assert!(people.contains("Dana") && people.contains("Family"));
    db.drop().await;
}

#[tokio::test]
async fn settings_and_passphrase_change() {
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = db.app();
    let mut b = Browser::new(&app);
    b.login("bob", "bob passphrase").await;
    let mut other = Browser::new(&app);
    other.login("bob", "bob passphrase").await;

    b.post("/settings/profile", &[("display_name", "Bob B.")]).await;
    assert!(b.get("/settings").await.body.contains("value=\"Bob B.\""));

    let r = b
        .post(
            "/settings/passphrase",
            &[("current", "nope"), ("new", "a new passphrase"), ("repeat", "a new passphrase")],
        )
        .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = b
        .post(
            "/settings/passphrase",
            &[("current", "bob passphrase"), ("new", "a new passphrase"), ("repeat", "different one")],
        )
        .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r =
        b.post("/settings/passphrase", &[("current", "bob passphrase"), ("new", "short"), ("repeat", "short")]).await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);
    let r = b
        .post(
            "/settings/passphrase",
            &[("current", "bob passphrase"), ("new", "a new passphrase"), ("repeat", "a new passphrase")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(r.body.contains("Passphrase changed"));
    assert_eq!(b.get("/settings").await.status, StatusCode::OK, "this browser has a fresh session");
    assert_eq!(location(&other.get("/settings").await), "/login", "the other browser is signed out");
    assert_eq!(Browser::new(&app).login("bob", "bob passphrase").await.status, StatusCode::UNAUTHORIZED);
    assert_eq!(Browser::new(&app).login("bob", "a new passphrase").await.status, StatusCode::SEE_OTHER);
    db.drop().await;
}

#[tokio::test]
async fn static_assets_are_cached_by_etag() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    for (path, ty) in [
        ("/static/app.css", "text/css"),
        ("/static/app.js", "text/javascript"),
        ("/static/htmx.min.js", "text/javascript"),
    ] {
        let r = send(&app, "GET", path, None, &[], "").await;
        assert_eq!(r.status, StatusCode::OK);
        assert_eq!(r.header("content-type").as_deref(), Some(ty));
        let etag = r.header("etag").unwrap();
        let again = send(&app, "GET", path, None, &[("if-none-match", &etag)], "").await;
        assert_eq!((again.status, again.body.as_str()), (StatusCode::NOT_MODIFIED, ""));
    }
    db.drop().await;
}

#[tokio::test]
async fn import_and_export_pages() {
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = db.app();
    let mut anna = Browser::new(&app);
    anna.login("anna", "anna passphrase").await;
    let page = anna.get("/import").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("Copy from Baikal") && page.body.contains("Download everything"));
    let mut bob = Browser::new(&app);
    bob.login("bob", "bob passphrase").await;
    assert!(!bob.get("/import").await.body.contains("Copy from Baikal"), "Baikal is for admins");
    let r = bob
        .post(
            "/import/baikal",
            &[("into", "00000000-0000-0000-0000-000000000000"), ("url", "http://x/"), ("user", "u"), ("password", "p")],
        )
        .await;
    assert_ne!(r.status, StatusCode::SEE_OTHER);

    // Upload a calendar as a browser does (multipart).
    let ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:x\r\nX-WR-CALNAME:Holidays\r\nBEGIN:VEVENT\r\nUID:h1\r\nDTSTAMP:20260101T000000Z\r\nDTSTART;VALUE=DATE:20260801\r\nSUMMARY:Beach\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
    let boundary = "XyZ123";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"target\"\r\n\r\nbyname\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"hol.ics\"\r\nContent-Type: text/calendar\r\n\r\n{ics}\r\n\
         --{boundary}--\r\n"
    );
    let cookie = format!("calzone_session={}", anna.cookie.clone().unwrap());
    let ct = format!("multipart/form-data; boundary={boundary}");
    let upload = |extra: Option<(&'static str, &'static str)>| {
        let mut h = vec![("cookie", cookie.as_str()), ("content-type", ct.as_str())];
        h.extend(extra);
        let (app, body) = (app.clone(), body.clone().into_bytes());
        let h: Vec<(String, String)> = h.into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
        async move {
            let h: Vec<(&str, &str)> = h.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
            common::send_bytes(&app, "POST", "/import/file", &h, body).await
        }
    };
    let (status, _, _) = upload(Some(("origin", "http://elsewhere.example"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "the Origin check covers uploads");
    let (status, headers2, _) = upload(None).await;
    assert_eq!(status, StatusCode::SEE_OTHER);
    let report = headers2.get("location").unwrap().to_str().unwrap().to_string();
    assert!(report.starts_with("/import/"));
    let mut page = anna.get(&report).await;
    for _ in 0..50 {
        if !page.body.contains("hx-trigger=\"every 2s\"") {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        page = anna.get(&report).await;
    }
    assert!(page.body.contains("Holidays") && page.body.contains("1 new"), "the report shows the result");
    assert!(!page.body.contains("every 2s"), "a finished report stops refreshing");
    assert_eq!(bob.get(&report).await.status, StatusCode::NOT_FOUND, "reports are private");
    assert!(anna.get("/import").await.body.contains("hol.ics"), "listed under recent imports");

    // Downloads: everything, and one collection.
    let (status, h, zip) = common::send_bytes(&app, "GET", "/export.zip", &[("cookie", &cookie)], Vec::new()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.get("content-type").unwrap(), "application/zip");
    assert!(
        h.get("content-disposition").unwrap().to_str().unwrap().starts_with("attachment; filename=\"calzone-anna-")
    );
    assert!(zip.starts_with(b"PK"));
    let cal = calzone_server::domain::collections::visible(
        &db.pool,
        principals::by_name(&db.pool, "anna").await.unwrap().id,
        calzone_server::domain::CollectionKind::Calendar,
    )
    .await
    .unwrap()
    .remove(0);
    let one = anna.get(&format!("/collections/{}/export", cal.id)).await;
    assert_eq!(one.header("content-type").as_deref(), Some("text/calendar; charset=utf-8"));
    assert!(one.body.contains("SUMMARY:Beach") && one.body.contains("X-WR-CALNAME:Holidays"));
    assert_eq!(bob.get(&format!("/collections/{}/export", cal.id)).await.status, StatusCode::NOT_FOUND);
    db.drop().await;
}

fn base32_decode(s: &str) -> Vec<u8> {
    let (mut buf, mut bits, mut out) = (0u32, 0, Vec::new());
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace()) {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'2'..=b'7' => c - b'2' + 26,
            _ => panic!("not base32"),
        };
        buf = (buf << 5) | u32::from(v);
        bits += 5;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

fn totp(secret: &[u8], offset: i64) -> String {
    use calzone_server::domain::two_factor::{code_at, step_at};
    format!("{:06}", code_at(secret, step_at(chrono::Utc::now().timestamp()) + offset))
}

#[tokio::test]
async fn two_factor_sign_in() {
    use calzone_server::config::TwoFactorPolicy;
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = calzone_server::router(calzone_server::AppState { two_factor: TwoFactorPolicy::Admins, ..db.state() });

    // An admin without two-factor reaches only the setup page.
    let mut other = Browser::new(&app);
    other.login("anna", "anna passphrase").await;
    let mut anna = Browser::new(&app);
    assert_eq!(location(&anna.login("anna", "anna passphrase").await), "/calendar");
    assert_eq!(location(&anna.get("/collections").await), "/settings/two-factor");
    assert_eq!(location(&anna.get("/").await), "/settings/two-factor");
    let htmx = send(
        &app,
        "GET",
        "/devices",
        None,
        &[("cookie", &format!("calzone_session={}", anna.cookie.clone().unwrap())), ("hx-request", "true")],
        "",
    )
    .await;
    assert_eq!(htmx.header("hx-redirect").as_deref(), Some("/settings/two-factor"));
    let page = anna.get("/settings/two-factor").await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(page.body.contains("<svg") && page.body.contains("needs two-factor"));
    let secret = base32_decode(&code_in(&page.body, "totp-secret"));
    assert_eq!(secret.len(), 20);
    assert_eq!(
        code_in(&anna.get("/settings/two-factor").await.body, "totp-secret"),
        code_in(&page.body, "totp-secret"),
        "a reload shows the same secret"
    );
    // A bob (not admin) is not held up under the admins policy.
    let mut bob = Browser::new(&app);
    bob.login("bob", "bob passphrase").await;
    assert_eq!(bob.get("/collections").await.status, StatusCode::OK);

    assert_eq!(anna.post("/settings/two-factor", &[("code", "000000")]).await.status, StatusCode::BAD_REQUEST);
    // Kept, so "a code works once" is tested with this exact code, whatever
    // 30-second step the clock has moved to since.
    let enroll_code = totp(&secret, 0);
    let done = anna.post("/settings/two-factor", &[("code", &enroll_code)]).await;
    assert_eq!(done.status, StatusCode::OK);
    let codes: Vec<String> =
        done.body.match_indices("<li><code>").map(|(i, _)| done.body[i + 10..i + 21].to_string()).collect();
    assert_eq!(codes.len(), 10);
    assert_eq!(anna.get("/collections").await.status, StatusCode::OK);
    assert_eq!(location(&other.get("/collections").await), "/login", "turning it on signs other browsers out");
    assert!(anna.get("/settings").await.body.contains("Recovery codes left: 10"));

    // Logging in now takes a code; the pending session reaches nothing else.
    anna.post("/logout", &[]).await;
    let r = anna.login("anna", "anna passphrase").await;
    assert_eq!(location(&r), "/login/code");
    assert!(r.header("set-cookie").unwrap().contains("Max-Age=600"));
    assert_eq!(location(&anna.get("/collections").await), "/login");
    assert_eq!(anna.get("/login/code").await.status, StatusCode::OK);
    let reused = anna.post("/login/code", &[("code", &enroll_code)]).await;
    assert_eq!(reused.status, StatusCode::UNAUTHORIZED, "a code works once");
    let ok = anna.post("/login/code", &[("code", &totp(&secret, 1))]).await;
    assert_eq!(location(&ok), "/calendar");
    assert_eq!(anna.get("/collections").await.status, StatusCode::OK);

    // A recovery code works once, upper case and spaces allowed.
    anna.post("/logout", &[]).await;
    anna.login("anna", "anna passphrase").await;
    let messy = codes[0].to_uppercase().replace('-', " ");
    assert_eq!(location(&anna.post("/login/code", &[("code", &messy)]).await), "/settings?recovery=used");
    assert!(anna.get("/settings?recovery=used").await.body.contains("Recovery codes left: 9"));
    anna.post("/logout", &[]).await;
    anna.login("anna", "anna passphrase").await;
    assert_eq!(anna.post("/login/code", &[("code", &codes[0])]).await.status, StatusCode::UNAUTHORIZED);

    // Wrong codes are budgeted; then the pending session is spent.
    // (The reused recovery code above was the first failure.)
    let mut statuses = Vec::new();
    for _ in 0..4 {
        statuses.push(anna.post("/login/code", &[("code", "123456")]).await.status.as_u16());
    }
    assert_eq!(statuses, [401, 401, 401, 401]);
    let blocked = anna.post("/login/code", &[("code", &totp(&secret, 0))]).await;
    assert_eq!(blocked.status, StatusCode::TOO_MANY_REQUESTS, "even a right code, once the budget is spent");
    assert_eq!(location(&anna.get("/login/code").await), "/login", "the pending session is gone");

    // Required for admins: it cannot be turned off, only reset by another admin.
    let anna_id = principals::by_name(&db.pool, "anna").await.unwrap().id;
    let fresh = calzone_server::router(calzone_server::AppState { two_factor: TwoFactorPolicy::Admins, ..db.state() });
    let mut a2 = Browser::new(&fresh);
    a2.login("anna", "anna passphrase").await;
    a2.post("/login/code", &[("code", &codes[1])]).await;
    let off = a2.post("/settings/two-factor/off", &[("passphrase", "anna passphrase"), ("code", &codes[2])]).await;
    assert_eq!(off.status, StatusCode::BAD_REQUEST);
    accounts::set_admin(&db.pool, principals::by_name(&db.pool, "bob").await.unwrap().id, true).await.unwrap();
    // Bob, now an admin without two-factor, is held at setup himself...
    let held = bob.post(&format!("/admin/people/{anna_id}/two-factor-reset"), &[]).await;
    assert_eq!(location(&held), "/settings/two-factor");
    // ...so the reset is done where the policy lets him in.
    let mut bob_opt = Browser { app: db.app(), cookie: bob.cookie.clone() };
    let r = bob_opt.post(&format!("/admin/people/{anna_id}/two-factor-reset"), &[]).await;
    assert_eq!(location(&r), "/admin/people");
    assert_eq!(location(&a2.get("/settings").await), "/login", "a reset signs them out");
    assert_eq!(location(&a2.login("anna", "anna passphrase").await), "/calendar", "no code step any more");
    assert_eq!(location(&a2.get("/collections").await), "/settings/two-factor", "and set up again");
    let cli = calzone_server::cli::run(&db.pool, &["user", "two-factor-reset", "anna"]).await.unwrap();
    assert!(cli.contains("reset"));

    // Everyone: a plain user is held too; optional: they may turn it off.
    let all = calzone_server::router(calzone_server::AppState { two_factor: TwoFactorPolicy::Everyone, ..db.state() });
    let mut carol = Browser::new(&all);
    let cid = principals::create_user(&db.pool, "carol", "").await.unwrap();
    accounts::set_passphrase(&db.pool, cid, "carol passphrase").await.unwrap();
    carol.login("carol", "carol passphrase").await;
    assert_eq!(location(&carol.get("/collections").await), "/settings/two-factor");
    let opt = db.app();
    let mut c2 = Browser::new(&opt);
    c2.login("carol", "carol passphrase").await;
    let s = base32_decode(&code_in(&c2.get("/settings/two-factor").await.body, "totp-secret"));
    assert_eq!(c2.post("/settings/two-factor", &[("code", &totp(&s, 0))]).await.status, StatusCode::OK);
    let off = c2.post("/settings/two-factor/off", &[("passphrase", "carol passphrase"), ("code", &totp(&s, 1))]).await;
    assert_eq!(location(&off), "/login");
    assert_eq!(location(&c2.login("carol", "carol passphrase").await), "/calendar", "off again");
    db.drop().await;
}

#[tokio::test]
async fn keep_me_signed_in() {
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = db.app();
    let expiry = |name: &'static str| {
        let pool = db.pool.clone();
        async move {
            let hours: f64 = sqlx::query_scalar(
                "SELECT EXTRACT(EPOCH FROM max(s.expires_at) - now())::float8 / 3600 FROM web_sessions s
                 JOIN principals p ON p.id = s.principal_id WHERE p.name = $1 AND s.verified",
            )
            .bind(name)
            .fetch_one(&pool)
            .await
            .unwrap();
            hours
        }
    };

    // Off (the default): a browser-session cookie and 12 hours on the server.
    let mut short = Browser::new(&app);
    let r = short.login("bob", "bob passphrase").await;
    let c = r.header("set-cookie").unwrap();
    assert!(!c.contains("Max-Age"), "{c}");
    let h = expiry("bob").await;
    assert!((11.9..=12.0).contains(&h), "{h}");
    assert!(Browser::new(&app).get("/login").await.body.contains("name=\"remember\""));

    // On: 30 days.
    let mut long = Browser::new(&app);
    let r = long.post("/login", &[("name", "anna"), ("passphrase", "anna passphrase"), ("remember", "on")]).await;
    assert!(r.header("set-cookie").unwrap().contains("Max-Age=2592000"));
    assert!(expiry("anna").await > 29.0 * 24.0);

    // A passphrase change keeps the short session short.
    let r = short
        .post(
            "/settings/passphrase",
            &[("current", "bob passphrase"), ("new", "bob passphrase 2"), ("repeat", "bob passphrase 2")],
        )
        .await;
    assert_eq!(r.status, StatusCode::OK);
    assert!(!r.header("set-cookie").unwrap().contains("Max-Age"));
    assert!(expiry("bob").await <= 12.0);

    // Through two-factor, the choice carries over to the full session.
    let s = base32_decode(&code_in(&long.get("/settings/two-factor").await.body, "totp-secret"));
    let done = long.post("/settings/two-factor", &[("code", &totp(&s, 0))]).await;
    let recovery = done.body[done.body.find("<li><code>").unwrap() + 10..][..11].to_string();
    let mut b = Browser::new(&app);
    let r = b.post("/login", &[("name", "anna"), ("passphrase", "anna passphrase"), ("remember", "on")]).await;
    assert_eq!(location(&r), "/login/code");
    let r = b.post("/login/code", &[("code", &totp(&s, 1))]).await;
    assert!(r.header("set-cookie").unwrap().contains("Max-Age=2592000"));
    let mut b = Browser::new(&app);
    b.login("anna", "anna passphrase").await;
    let r = b.post("/login/code", &[("code", &recovery)]).await;
    assert_eq!(location(&r), "/settings?recovery=used");
    assert!(!r.header("set-cookie").unwrap().contains("Max-Age"));
    db.drop().await;
}

/// The visible words of a page: tags, attributes and scripts dropped.
fn visible_words(html: &str) -> Vec<String> {
    let starts = |i: usize, pat: &str| html.get(i..i + pat.len()).is_some_and(|x| x.eq_ignore_ascii_case(pat));
    let mut out = String::new();
    let (mut in_tag, mut skip) = (false, false);
    for (i, c) in html.char_indices() {
        if starts(i, "<script") || starts(i, "<style") {
            skip = true;
        }
        if skip && (starts(i, "</script>") || starts(i, "</style>")) {
            skip = false;
        }
        match c {
            '<' => in_tag = true,
            '>' => {
                in_tag = false;
                out.push(' ');
            }
            _ if !in_tag && !skip => out.push(c),
            _ => {}
        }
    }
    out.split(|c: char| !c.is_alphabetic() && c != '\'').filter(|w| !w.is_empty()).map(str::to_string).collect()
}

#[tokio::test]
async fn every_page_in_dutch_and_preferences() {
    let Some(db) = Db::new().await else { return };
    setup(&db).await;
    let app = db.app();
    let mut anna = Browser::new(&app);
    anna.login("anna", "anna passphrase").await;
    anna.post("/collections", &[("kind", "calendar"), ("name", "Gezin")]).await;
    anna.post("/collections", &[("kind", "addressbook"), ("name", "Adressen")]).await;
    // Preferences: Dutch, light, Sunday, 12-hour, a zone.
    let r = anna
        .post(
            "/settings/preferences",
            &[
                ("language", "nl"),
                ("theme", "light"),
                ("timezone", "Europe/Amsterdam"),
                ("week_start", "7"),
                ("clock", "12"),
            ],
        )
        .await;
    assert_eq!(location(&r), "/settings?saved=preferences");
    let s = anna.get("/settings?saved=preferences").await.body;
    assert!(s.contains(r#"<html lang="nl" data-theme="light">"#) && s.contains("Voorkeuren opgeslagen."));
    assert!(s.contains(r#"<option value="7" selected>"#) && s.contains(r#"<option value="12" selected>"#));
    assert!(s.contains(r#"<option value="Europe/Amsterdam" selected>"#));
    assert_eq!(
        anna.post(
            "/settings/preferences",
            &[
                ("language", "nl"),
                ("theme", "pink"),
                ("timezone", "Europe/Amsterdam"),
                ("week_start", "1"),
                ("clock", "24")
            ]
        )
        .await
        .status,
        StatusCode::BAD_REQUEST
    );

    // Every page in Dutch, with no common English words left in sight.
    let english = [
        "Save",
        "Delete",
        "Settings",
        "Calendars",
        "Address",
        "books",
        "Remove",
        "Shared",
        "Download",
        "Import",
        "Export",
        "Devices",
        "People",
        "Groups",
        "Add",
        "Your",
        "Passphrase",
        "Copy",
        "Done",
        "Contacts",
        "Name",
        "the",
        "and",
        "you",
    ];
    let pages = [
        "/settings",
        "/collections",
        "/devices",
        "/admin/people",
        "/import",
        "/calendar",
        "/contacts",
        "/contacts/new",
    ];
    for path in pages {
        let r = anna.get(path).await;
        assert_eq!(r.status, StatusCode::OK, "{path}");
        assert!(r.body.contains(r#"lang="nl""#), "{path}");
        let words = visible_words(&r.body);
        let left: Vec<&str> = english.iter().copied().filter(|e| words.iter().any(|w| w == e)).collect();
        assert!(left.is_empty(), "{path} still shows English: {left:?}");
    }
    let id = calzone_server::domain::collections::visible(
        &db.pool,
        principals::by_name(&db.pool, "anna").await.unwrap().id,
        calzone_server::domain::CollectionKind::Calendar,
    )
    .await
    .unwrap()[0]
        .id;
    for path in [format!("/collections/{id}"), format!("/calendar/settings/{id}")] {
        let r = anna.get(&path).await;
        let words = visible_words(&r.body);
        let left: Vec<&str> = english.iter().copied().filter(|e| words.iter().any(|w| w == e)).collect();
        assert!(left.is_empty(), "{path} still shows English: {left:?}");
    }
    // Generic errors too, in the reader's language.
    let gone = format!("/collections/{}", uuid::Uuid::new_v4());
    let r = send(
        &app,
        "GET",
        &gone,
        None,
        &[("cookie", &format!("calzone_session={}", anna.cookie.clone().unwrap())), ("hx-request", "true")],
        "",
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    assert!(r.body.contains("Niet gevonden.") && r.header("x-cz-generic").is_none());
    let page = anna.get(&gone).await.body;
    assert!(page.contains("Niet gevonden.") && page.contains("Gebruik de terugknop"));
    // Bob is not an admin: no People page, in his (browser) language.
    let mut bob = Browser::new(&app);
    bob.login("bob", "bob passphrase").await;
    let r = bob.get("/admin/people").await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert!(r.body.contains("Only an admin can do that."));
    // Calendar and Contacts in the bar, the rest behind Settings (decision 53).
    let s = anna.get("/calendar").await.body;
    assert!(s.contains("class=\"settings-link\"") && s.contains(">Instellingen<"));
    assert!(
        !s.contains("class=\"subnav\"") && !s.contains("href=\"/devices\""),
        "settings' sections stay behind the gear"
    );
    let s = anna.get("/devices").await.body;
    assert!(
        s.contains("class=\"subnav\"") && s.contains("href=\"/admin/people\"") && !s.contains("action=\"/logout\"")
    );
    assert!(anna.get("/settings").await.body.contains("class=\"settings-logout\""), "log out at the bottom of Account");
    assert!(s.contains("class=\"settings-link\" href=\"/settings\" aria-current=\"page\""));
    let s = bob.get("/settings").await.body;
    assert!(s.contains("class=\"subnav\"") && !s.contains("href=\"/admin/people\""), "People only for admins");
    db.drop().await;
}

#[tokio::test]
async fn the_calendar_list_and_its_settings_sheet() {
    let Some(db) = Db::new().await else { return };
    let (anna_id, bob_id) = setup(&db).await;
    let app = db.app();
    let mut anna = Browser::new(&app);
    anna.login("anna", "anna passphrase").await;
    anna.post("/collections", &[("kind", "calendar"), ("name", "Family")]).await;
    let fam = calzone_server::domain::collections::by_path(&db.pool, anna_id, "anna", "family").await.unwrap().id;
    calzone_server::domain::collections::set_grant(
        &db.pool,
        anna_id,
        fam,
        bob_id,
        Some(calzone_server::domain::Access::Read),
    )
    .await
    .unwrap();
    let page = anna.get("/calendar").await.body;
    assert!(page.contains(&format!("data-cal-id=\"{fam}\"")) && page.contains("data-cal-show checked"));
    let sheet = anna.get(&format!("/calendar/settings/{fam}")).await.body;
    assert!(
        sheet.contains("My name for it") && sheet.contains("Shared with") && sheet.contains("Bob")
            || sheet.contains("bob")
    );
    assert!(sheet.contains(&format!("/collections/{fam}/unshare/{bob_id}")));
    // Bob: his own settings, no sharing; hidden shows unchecked.
    let mut bob = Browser::new(&app);
    bob.login("bob", "bob passphrase").await;
    calzone_server::domain::prefs::set_calendar_prefs(
        &db.pool,
        bob_id,
        fam,
        &calzone_server::domain::prefs::CalendarPrefs {
            hidden: true,
            display_name: Some("Anna's".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let page = bob.get("/calendar").await.body;
    assert!(
        page.contains("Anna&#39;s") && !page.contains("data-cal-show checked") && page.contains("title=\"From Anna\"")
    );
    let sheet = bob.get(&format!("/calendar/settings/{fam}")).await.body;
    assert!(sheet.contains("My name for it") && !sheet.contains("Shared with") && sheet.contains("can view"));
    db.drop().await;
}
