//! The `/sync/v1` wire contract (decision 47), the wordwarz way: every
//! answer the app decodes is captured in `tests/golden/*.json`, and the
//! app's contract test parses these files with its own models. Regenerate
//! on purpose only: `UPDATE_GOLDEN=1 cargo test --test golden_pg`. Without
//! it, the live answers must keep the stored files' shape (the same keys at
//! every level), so a server change that would break the app fails here
//! first. Needs `CALZONE_TEST_DATABASE_URL`.

mod common;

use std::path::PathBuf;

use axum::http::StatusCode;
use calzone_core::sync::{Changed, Notice, State};
use calzone_server::domain::prefs::{self, CalendarPrefs};
use calzone_server::domain::{
    Access, CollectionKind, NewCollection, Precondition, accounts, collections, objects, principals,
};
use common::{Db, send};
use serde_json::{Value, json};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden")
}

/// Tags of tagged unions: a field conflict and a lost delete differ in keys.
const TAGS: [&str; 1] = ["kind"];

/// Same keys at every level; for arrays, the first element's shape.
fn same_shape(path: &str, a: &Value, b: &Value) -> Result<(), String> {
    match (a, b) {
        (Value::Object(x), Value::Object(y)) if TAGS.iter().any(|t| x.contains_key(*t) && x.get(*t) != y.get(*t)) => {
            Ok(())
        }
        (Value::Object(x), Value::Object(y)) => {
            let kx: Vec<_> = x.keys().collect();
            let ky: Vec<_> = y.keys().collect();
            if kx != ky {
                return Err(format!("{path}: keys {kx:?} != {ky:?}"));
            }
            for (k, v) in x {
                same_shape(&format!("{path}.{k}"), v, &y[k])?;
            }
            Ok(())
        }
        (Value::Array(x), Value::Array(y)) => match (x.first(), y.first()) {
            (Some(p), Some(q)) => same_shape(&format!("{path}[0]"), p, q),
            _ => Ok(()),
        },
        (Value::Null, _) | (_, Value::Null) => Ok(()),
        (x, y) if std::mem::discriminant(x) == std::mem::discriminant(y) => Ok(()),
        (x, y) => Err(format!("{path}: {x} vs {y}")),
    }
}

/// Device tokens from the throwaway database become `czd_example000...`,
/// so nothing in the repo looks like a credential.
fn redact(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("czd_") {
        out.push_str(&rest[..at + 4]);
        out.push_str("example000000000000000000000000000000000000");
        rest = rest[at + 4..].trim_start_matches(|c: char| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    }
    out.push_str(rest);
    out
}

fn golden(name: &str, v: &Value) {
    let file = golden_dir().join(format!("{name}.json"));
    if std::env::var("UPDATE_GOLDEN").is_ok() {
        std::fs::create_dir_all(golden_dir()).unwrap();
        std::fs::write(&file, redact(&serde_json::to_string_pretty(v).unwrap()) + "\n").unwrap();
        return;
    }
    let stored: Value = serde_json::from_str(
        &std::fs::read_to_string(&file)
            .unwrap_or_else(|_| panic!("{} missing: run UPDATE_GOLDEN=1 cargo test --test golden_pg", file.display())),
    )
    .unwrap();
    if let Err(e) = same_shape(name, &stored, v) {
        panic!("{name} changed shape ({e}); if on purpose, regenerate and update the app's models");
    }
}

fn event(uid: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//CalZone//EN\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\n\
         DTSTART;TZID=Europe/Amsterdam:20260310T090000\r\nDTEND;TZID=Europe/Amsterdam:20260310T100000\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

#[tokio::test]
async fn the_sync_wire_contract() {
    let Some(db) = Db::new().await else { return };
    let app = db.app();
    // A made-up household, like the README screenshots.
    let mila = principals::create_user(&db.pool, "mila", "Mila").await.unwrap();
    let jonas = principals::create_user(&db.pool, "jonas", "Jonas").await.unwrap();
    let family = collections::create(
        &db.pool,
        jonas,
        NewCollection {
            kind: CollectionKind::Calendar,
            slug: "family",
            display_name: "Family",
            description: "Everyone's plans",
            color: Some("#3a7bd5"),
            components: &["VEVENT"],
        },
    )
    .await
    .unwrap()
    .id;
    collections::set_grant(&db.pool, jonas, family, mila, Some(Access::Write)).await.unwrap();
    let p = CalendarPrefs { color: Some("#d53a7b".into()), default_alarm: Some(30), ..Default::default() };
    prefs::set_calendar_prefs(&db.pool, mila, family, &p).await.unwrap();
    let swim = event("swim", "SUMMARY:Swimming\r\nLOCATION:Pool\r\n");
    objects::put(&db.pool, jonas, family, "swim.ics", &swim, Precondition::None).await.unwrap();
    objects::put(&db.pool, jonas, family, "gone.ics", &event("gone", "SUMMARY:Old\r\n"), Precondition::None)
        .await
        .unwrap();

    let web = format!("calzone_session={}", accounts::create_session(&db.pool, mila, true).await.unwrap());
    let page = send(&app, "POST", "/devices/link", None, &[("cookie", &web)], "").await.body;
    let at = page.find("czl_").unwrap();
    let claim = json!({ "code": &page[at..at + 47], "device": "Pixel 9" }).to_string();
    let r = send(&app, "POST", "/sync/v1/link", None, &[("content-type", "application/json")], &claim).await;
    let signed_in: Value = serde_json::from_str(&r.body).unwrap();
    golden("signed_in", &signed_in);
    let token = format!("Bearer {}", signed_in["token"].as_str().unwrap());
    let call = |method: &'static str, path: String, body: Option<Value>| {
        let app = app.clone();
        let token = token.clone();
        async move {
            let h = [("authorization", token.as_str()), ("content-type", "application/json")];
            let r = send(&app, method, &path, None, &h, &body.map(|b| b.to_string()).unwrap_or_default()).await;
            (r.status, serde_json::from_str::<Value>(&r.body).unwrap_or(Value::Null))
        }
    };

    let (_, me) = call("GET", "/sync/v1/me".into(), None).await;
    golden("me", &me);
    let (_, list) = call("GET", "/sync/v1/collections".into(), None).await;
    golden("collections", &list);

    let (_, full) = call("GET", format!("/sync/v1/collections/{family}/changes"), None).await;
    golden("changes_full", &full);
    let since = full["state"].as_i64().unwrap();
    let swim_change = full["changes"].as_array().unwrap().iter().find(|c| c["name"] == "swim.ics").unwrap();
    let mila_swim = swim_change["body"].as_str().unwrap().to_string();
    objects::delete(&db.pool, jonas, family, "gone.ics", Precondition::None).await.unwrap();
    let moved = swim.replace("LOCATION:Pool", "LOCATION:Outdoor pool");
    objects::put(&db.pool, jonas, family, "swim.ics", &moved, Precondition::None).await.unwrap();
    let (_, delta) = call("GET", format!("/sync/v1/collections/{family}/changes?since={since}&limit=1"), None).await;
    assert!(delta["more"].as_bool().unwrap());
    let (_, rest) = call("GET", format!("/sync/v1/collections/{family}/changes?since={}", delta["state"]), None).await;
    let mut both = rest.clone();
    // A change first (the shape check reads the first element), then a tombstone.
    both["changes"] = json!([rest["changes"][0], delta["changes"][0]]);
    assert!(both["changes"][1].get("body").is_none());
    golden("changes", &both);

    // An upload: one merged with a reported conflict, one refused.
    let mine =
        mila_swim.replace("SUMMARY:Swimming", "SUMMARY:Swim lesson").replace("LOCATION:Pool", "LOCATION:Sports hall");
    let upload = json!({ "changes": [
        { "name": "swim.ics", "base": mila_swim, "body": mine },
        { "name": "card.ics", "body": "BEGIN:VCARD\r\nVERSION:4.0\r\nFN:x\r\nEND:VCARD\r\n" },
    ] });
    golden("upload", &upload);
    let (s, answer) = call("POST", format!("/sync/v1/collections/{family}/changes"), Some(upload)).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(answer["pushed"][0]["conflicts"][0]["lost"]["kind"], "field");
    assert_eq!(answer["rejected"][0]["reason"], "invalid");
    golden("answer", &answer);

    let (s, error) = call("GET", format!("/sync/v1/collections/{family}/changes?since=999"), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    golden("error", &error);

    let notice = Notice { collections: vec![Changed { id: family.to_string(), state: State(7) }], all: false };
    golden("notice", &serde_json::to_value(&notice).unwrap());
    golden(
        "push_endpoint",
        &json!({ "endpoint": "https://ntfy.example.com/upAbCdEf?up=1", "key": "BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4", "auth": "BTBZMqHH6r4Tts7J_aSIgg" }),
    );
    db.drop().await;
}
