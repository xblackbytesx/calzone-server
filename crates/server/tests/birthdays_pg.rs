//! Birthdays from contacts against a real Postgres (decision 57): the
//! calendar turned on and kept up to date, the same person on two
//! people's cards shown once, birthdays-only sharing that opens nothing
//! else, read-only through every door, the person's own reminder, and the
//! manual birthday events removed (dates copied first) with one Undo.

mod common;

use axum::Router;
use axum::http::StatusCode;
use calzone_server::domain::birthdays;
use calzone_server::domain::feeds;
use calzone_server::domain::{
    Access, CollectionKind, DomainError, NewCollection, Precondition, accounts, app_passwords, collections, objects,
    principals,
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

fn card(uid: &str, name: &str, extra: &str) -> String {
    format!("BEGIN:VCARD\r\nVERSION:3.0\r\nUID:{uid}\r\nFN:{name}\r\n{extra}END:VCARD\r\n")
}

fn yearly(uid: &str, title: &str, start: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20200101T000000Z\r\n\
         DTSTART;VALUE=DATE:{start}\r\nRRULE:FREQ=YEARLY\r\nSUMMARY:{title}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

async fn book(db: &Db, owner: Uuid, slug: &str) -> Uuid {
    let new = NewCollection {
        kind: CollectionKind::AddressBook,
        slug,
        display_name: slug,
        description: "",
        color: None,
        components: &[],
    };
    collections::create(&db.pool, owner, new).await.unwrap().id
}

/// Every event's text in a Birthdays calendar, as its owner reads it.
async fn texts(db: &Db, who: Uuid, cal: Uuid) -> Vec<String> {
    objects::all(&db.pool, who, cal).await.unwrap().into_iter().map(|s| s.body).collect()
}

fn titled<'a>(all: &'a [String], name: &str) -> Vec<&'a String> {
    all.iter().filter(|t| t.contains(&format!("SUMMARY:{name}\r\n"))).collect()
}

#[tokio::test]
async fn birthdays_from_contacts() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    let (anna, anna_id) = Who::new(&db, &app, "anna").await;
    let (wendy, wendy_id) = Who::new(&db, &app, "wendy").await;
    let mine = book(&db, anna_id, "contacts").await;
    let hers = book(&db, wendy_id, "hers").await;
    for (n, t) in [
        ("ada.vcf", card("ada", "Ada Lovelace", "N:Lovelace;Ada;;;\r\nBDAY:19851203\r\n")),
        ("oma.vcf", card("oma", "Oma", "TEL:+31 6 1111 2222\r\nBDAY:19400101\r\n")),
        ("piet.vcf", card("piet", "Piet Jansen", "N:Jansen;Piet;;;\r\n")),
        ("cy.vcf", card("cy", "Cy Young", "BDAY:19800101\r\n")),
    ] {
        objects::put(&db.pool, anna_id, mine, n, &t, Precondition::None).await.unwrap();
    }
    for (n, t) in [
        // The same person as anna's Oma: same day, same number, another name.
        ("gerda.vcf", card("gerda", "Gerda de Vries", "TEL:06-11112222\r\nBDAY:--0101\r\n")),
        ("friend.vcf", card("friend", "Wendy Friend", "TEL:+31 6 9999 0000\r\nNOTE:secret note\r\nBDAY:19900505\r\n")),
    ] {
        objects::put(&db.pool, wendy_id, hers, n, &t, Precondition::None).await.unwrap();
    }
    let cal_new = NewCollection {
        kind: CollectionKind::Calendar,
        slug: "personal",
        display_name: "Personal",
        description: "",
        color: None,
        components: &[],
    };
    let personal = collections::create(&db.pool, anna_id, cal_new).await.unwrap().id;
    for (n, t) in [
        ("m1.ics", yearly("m1", "Verjaardag Ada Lovelace", "19851203")),
        ("m2.ics", yearly("m2", "Piet Jansen jarig", "19700707")),
        ("m3.ics", yearly("m3", "Cy Young birthday", "19800202")),
        ("m4.ics", yearly("m4", "Dentist", "20200303")),
    ] {
        objects::put(&db.pool, anna_id, personal, n, &t, Precondition::None).await.unwrap();
    }

    // Off: offered under the calendar list, explained on its page.
    assert!(anna.get("/calendar").await.body.contains("href=\"/birthdays\">+ Birthdays from contacts"));
    assert!(anna.get("/birthdays").await.body.contains("Turn on Birthdays"));
    assert_eq!(anna.get("/birthdays/cleanup").await.status, StatusCode::SEE_OTHER, "needs Birthdays on");

    // On: anna's own birthdays, the age per year in the grid.
    let r = anna.post("/birthdays/on", &[]).await;
    assert_eq!(loc(&r), "/birthdays");
    let cal = birthdays::of(&db.pool, anna_id).await.unwrap().unwrap().collection;
    assert!(!anna.get("/calendar").await.body.contains("+ Birthdays from contacts"));
    let all = texts(&db, anna_id, cal).await;
    assert_eq!(all.len(), 3, "Ada, Oma, Cy");
    let grid = anna.get("/api/occurrences?from=2026-12-01&to=2027-01-01").await.body;
    assert!(grid.contains("\"title\":\"Ada Lovelace (41)\""), "{grid}");
    let page = anna.get("/birthdays").await.body;
    assert!(page.contains("contacts") && page.contains("yours") && page.contains("Check and remove"), "{page}");
    assert!(!birthdays::rebuild(&db.pool, cal).await.unwrap(), "nothing changed, nothing written");

    // Read-only and personal through every door.
    let ada = titled(&all, "Ada Lovelace")[0].clone();
    let ada_name = objects::all(&db.pool, anna_id, cal)
        .await
        .unwrap()
        .into_iter()
        .find(|s| s.body.contains("SUMMARY:Ada Lovelace\r\n"))
        .unwrap()
        .meta
        .name;
    assert!(matches!(
        objects::put(&db.pool, anna_id, cal, "x.ics", &yearly("x", "Mine", "20200101"), Precondition::None).await,
        Err(DomainError::ReadOnly)
    ));
    assert!(matches!(
        collections::set_grant(&db.pool, anna_id, cal, wendy_id, Some(Access::Read)).await,
        Err(DomainError::Invalid(_))
    ));
    assert!(matches!(
        feeds::create_link(&db.pool, anna_id, cal, "", calzone_core::feed::Detail::Full).await,
        Err(DomainError::ReadOnly)
    ));
    assert!(matches!(collections::get(&db.pool, wendy_id, cal).await, Err(DomainError::NotFound)));
    let (_, pw) = app_passwords::create(&db.pool, anna_id, "phone", None).await.unwrap();
    let basic = Some(("anna", pw.as_str()));
    let dav = format!("/dav/calendars/anna/{}/", collections::get(&db.pool, anna_id, cal).await.unwrap().slug);
    let put = send(&app, "PUT", &format!("{dav}new.ics"), basic, &[("content-type", "text/calendar")], &ada).await;
    assert_eq!(put.status, StatusCode::FORBIDDEN, "{}", put.body);
    let props = "<?xml version=\"1.0\"?><propfind xmlns=\"DAV:\"><prop><current-user-privilege-set/></prop></propfind>";
    let pf = send(&app, "PROPFIND", &dav, basic, &[("depth", "0")], props).await;
    assert!(pf.body.contains("read") && !pf.body.contains("write"), "{}", pf.body);
    let sheet = anna.get(&format!("/calendar/settings/{cal}")).await.body;
    assert!(sheet.contains("Birthdays settings") && !sheet.contains("cal-share") && !sheet.contains("id=\"cal-link\""));
    // The panel links the contact.
    let panel = anna.get(&format!("/calendar/event?c={cal}&n={}", enc(&ada_name))).await.body;
    assert!(panel.contains(&format!("href=\"/contacts/{mine}/ada.vcf\"")) && panel.contains("Open contact"), "{panel}");

    // Wendy shares her birthdays only: Gerda is anna's Oma (once), Wendy
    // Friend arrives with nothing but a name and a date.
    let r = wendy.post(&format!("/collections/{hers}/birthdays/share"), &[("principal", &anna_id.to_string())]).await;
    assert_eq!(loc(&r), format!("/collections/{hers}"));
    assert!(wendy.get(&format!("/collections/{hers}")).await.body.contains("Share birthdays only"));
    assert!(
        anna.post(&format!("/collections/{hers}/birthdays/share"), &[("principal", &anna_id.to_string())])
            .await
            .status
            .is_client_error()
    );
    assert_eq!(birthdays::rebuild_changed(&db.pool).await.unwrap(), 1);
    let all = texts(&db, anna_id, cal).await;
    assert_eq!(all.len(), 4, "Oma once, Wendy Friend added");
    assert_eq!(titled(&all, "Oma").len(), 1);
    assert!(titled(&all, "Gerda de Vries").is_empty(), "anna's own card is the one shown");
    let friend = titled(&all, "Wendy Friend")[0];
    assert!(!friend.contains("X-CALZONE-CONTACT") && !friend.contains("secret") && !friend.contains("9999"));
    assert_eq!(anna.get(&format!("/contacts/{hers}/friend.vcf")).await.status, StatusCode::NOT_FOUND);
    assert!(!anna.get("/contacts").await.body.contains("Wendy Friend"));
    assert!(matches!(objects::get(&db.pool, anna_id, hers, "friend.vcf").await, Err(DomainError::NotFound)));
    assert!(anna.get("/birthdays").await.body.contains("birthdays only, from wendy"));

    // A changed card changes only its own event.
    let before = objects::list(&db.pool, anna_id, cal).await.unwrap();
    objects::put(&db.pool, anna_id, mine, "cy.vcf", &card("cy", "Cy Young", "BDAY:19800303\r\n"), Precondition::None)
        .await
        .unwrap();
    assert!(birthdays::rebuild(&db.pool, cal).await.unwrap());
    let after = objects::list(&db.pool, anna_id, cal).await.unwrap();
    let unchanged =
        before.iter().filter(|b| after.iter().any(|a| a.name == b.name && a.revision == b.revision)).count();
    assert_eq!(unchanged, 3, "only Cy's event was written");

    // Options: a reminder (anna's own), anniversaries, wendy's book left out.
    objects::put(
        &db.pool,
        anna_id,
        mine,
        "wed.vcf",
        &card("wed", "Ada and Bram", "ANNIVERSARY:20100605\r\n"),
        Precondition::None,
    )
    .await
    .unwrap();
    let r =
        anna.post("/birthdays", &[("reminder", "-540"), ("anniversaries", "on"), ("book", &mine.to_string())]).await;
    assert_eq!(loc(&r), "/birthdays?saved=1");
    let all = texts(&db, anna_id, cal).await;
    assert!(titled(&all, "Wendy Friend").is_empty() && titled(&all, "Oma").len() == 1);
    assert_eq!(titled(&all, "Ada and Bram\\, wedding anniversary").len(), 1, "{all:?}");
    assert!(all.iter().all(|t| t.contains("TRIGGER:PT540M")), "the reminder on every event");
    let shared: Vec<String> = sqlx::query_scalar("SELECT body FROM objects WHERE collection_id = $1")
        .bind(cal)
        .fetch_all(&db.pool)
        .await
        .unwrap();
    assert!(shared.iter().all(|b| !b.contains("VALARM")), "reminders stay in anna's layer");
    assert!(anna.post("/birthdays", &[("reminder", "123")]).await.status.is_client_error());

    // The manual ones: Ada's is covered, Piet's date goes to his card,
    // Cy's differs (left alone), the dentist is not a birthday.
    let c = anna.get("/birthdays/cleanup").await.body;
    assert!(c.contains("Verjaardag Ada Lovelace") && c.contains("In Birthdays as Ada Lovelace"), "{c}");
    assert!(c.contains("Piet Jansen has no birthday yet") && c.contains("value=\"1970-07-07\""));
    assert!(c.contains("Not removed: Cy Young has a different birthday"));
    assert!(!c.contains("Dentist"));
    assert_eq!(c.matches("name=\"pick\"").count(), 2);
    let etag = |c: Uuid, n: &'static str| {
        let pool = db.pool.clone();
        async move { objects::get(&pool, anna_id, c, n).await.unwrap().meta.etag() }
    };
    let (k1, k2) = (format!("{personal}/m1.ics"), format!("{personal}/m2.ics"));
    let (e1, e2, ep) = (etag(personal, "m1.ics").await, etag(personal, "m2.ics").await, etag(mine, "piet.vcf").await);
    let piet = format!("{mine}/piet.vcf");
    let mut form: Vec<(String, String)> = vec![
        ("pick".into(), k1.clone()),
        (format!("etag:{k1}"), e1.clone()),
        ("pick".into(), k2.clone()),
        (format!("etag:{k2}"), e2.clone()),
        (format!("card:{k2}"), piet.clone()),
        (format!("cardetag:{k2}"), ep.clone()),
        (format!("date:{k2}"), "1970-07-07".into()),
    ];
    let as_ref = |f: &Vec<(String, String)>| f.iter().map(|(a, b)| (a.clone(), b.clone())).collect::<Vec<_>>();
    let post = |f: Vec<(String, String)>| {
        let anna = &anna;
        async move {
            let fields: Vec<(&str, &str)> = f.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
            anna.post("/birthdays/cleanup", &fields).await
        }
    };
    assert_eq!(post(as_ref(&form)).await.status, StatusCode::BAD_REQUEST, "needs the box ticked");
    form.push(("confirm".into(), "yes".into()));
    // A stale ETag: nothing at all happens.
    let mut stale = as_ref(&form);
    stale[1].1 = "\"999\"".into();
    assert_eq!(loc(&post(stale).await), "/birthdays/cleanup?stale=1");
    assert_eq!(objects::list(&db.pool, anna_id, personal).await.unwrap().len(), 4);
    let r = post(as_ref(&form)).await;
    let done = loc(&r);
    assert!(done.starts_with("/birthdays/cleanup?change=") && done.ends_with("&n=2&copied=1"), "{done}");
    let names: Vec<String> =
        objects::list(&db.pool, anna_id, personal).await.unwrap().into_iter().map(|m| m.name).collect();
    assert_eq!(names, vec!["m3.ics", "m4.ics"]);
    assert!(objects::get(&db.pool, anna_id, mine, "piet.vcf").await.unwrap().body.contains("BDAY:1970-07-07"));
    assert_eq!(titled(&texts(&db, anna_id, cal).await, "Piet Jansen").len(), 1, "rebuilt at once");
    let p = anna.get(&done).await.body;
    assert!(p.contains("Removed 2 events. Copied 1 birthday to a contact.") && p.contains("/birthdays/undo/"), "{p}");
    // One Undo: both events back, Piet without a birthday again.
    let change = done.split("change=").nth(1).unwrap().split('&').next().unwrap().to_string();
    let r = anna.post(&format!("/birthdays/undo/{change}"), &[]).await;
    assert_eq!(loc(&r), "/birthdays/cleanup?undone=1");
    assert_eq!(objects::list(&db.pool, anna_id, personal).await.unwrap().len(), 4);
    assert!(!objects::get(&db.pool, anna_id, mine, "piet.vcf").await.unwrap().body.contains("BDAY"));
    assert!(titled(&texts(&db, anna_id, cal).await, "Piet Jansen").is_empty());

    // Dutch pages read Dutch.
    let nl =
        send(&app, "GET", "/birthdays", None, &[("cookie", &anna.cookie), ("accept-language", "nl")], "").await.body;
    assert!(nl.contains("Verjaardagen") && nl.contains("Ook trouwdagen") && !nl.contains("Wedding"), "{nl}");

    // Off: only with the box ticked; the calendar goes, contacts stay.
    assert_eq!(anna.post("/birthdays/off", &[]).await.status, StatusCode::BAD_REQUEST);
    let r = anna.post("/birthdays/off", &[("confirm", "yes")]).await;
    assert_eq!(loc(&r), "/birthdays");
    assert!(birthdays::of(&db.pool, anna_id).await.unwrap().is_none());
    assert!(matches!(collections::get(&db.pool, anna_id, cal).await, Err(DomainError::NotFound)));
    assert_eq!(objects::list(&db.pool, anna_id, mine).await.unwrap().len(), 5);
    let _ = wendy;
    db.drop().await;
}
