//! The domain service against a real Postgres: conditional writes, the
//! per-user layer, access (404 for the unseen), the change log, range
//! candidates, and the row lock under concurrency.

mod common;

use calzone_server::domain::{
    Access, CollectionKind, DomainError, NewCollection, Precondition, collections, objects, principals,
};
use chrono::NaiveDateTime;
use sqlx::PgPool;
use uuid::Uuid;

fn dt(s: &str) -> NaiveDateTime {
    NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
}

fn event(uid: &str, start: &str, extra: &str) -> String {
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nDTSTAMP:20260101T000000Z\r\nSUMMARY:Synthetic {uid}\r\nDTSTART:{start}\r\nDURATION:PT1H\r\n{extra}END:VEVENT\r\nEND:VCALENDAR\r\n"
    )
}

const ALARM: &str = "BEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:Reminder\r\nTRIGGER:-PT15M\r\nEND:VALARM\r\n";

async fn calendar(pool: &PgPool, owner: Uuid, slug: &str) -> Uuid {
    collections::create(
        pool,
        owner,
        NewCollection {
            kind: CollectionKind::Calendar,
            slug,
            display_name: "Family",
            description: "",
            color: Some("#3366cc"),
            components: &["VEVENT", "VTODO"],
        },
    )
    .await
    .unwrap()
    .id
}

async fn addressbook(pool: &PgPool, owner: Uuid) -> Uuid {
    collections::create(
        pool,
        owner,
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
    .id
}

#[tokio::test]
async fn put_get_round_trip_and_preconditions() {
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "me", "Me").await.unwrap();
    let cal = calendar(p, me, "family").await;
    let body = event("a", "20260601T090000Z", "");

    let w = objects::put(p, me, cal, "a.ics", &body, Precondition::IfNoneMatchAny).await.unwrap();
    assert!(w.created);
    assert_eq!(w.meta.etag(), "\"1\"");
    let got = objects::get(p, me, cal, "a.ics").await.unwrap();
    assert_eq!(got.body, body, "stored byte for byte");

    // Creating again must not overwrite.
    let e = objects::put(p, me, cal, "a.ics", &body, Precondition::IfNoneMatchAny).await;
    assert!(matches!(e, Err(DomainError::PreconditionFailed)));
    // A stale ETag is refused, the current one accepted.
    let changed = event("a", "20260601T100000Z", "");
    let e = objects::put(p, me, cal, "a.ics", &changed, Precondition::IfMatch("\"999\"".into())).await;
    assert!(matches!(e, Err(DomainError::PreconditionFailed)));
    let w = objects::put(p, me, cal, "a.ics", &changed, Precondition::IfMatch(w.meta.etag())).await.unwrap();
    assert!(!w.created);
    assert_eq!(w.meta.revision, 2);
    // If-Match on something missing fails.
    let e = objects::put(p, me, cal, "nope.ics", &body, Precondition::IfMatch("\"1\"".into())).await;
    assert!(matches!(e, Err(DomainError::PreconditionFailed)));

    // Delete with a stale ETag fails, with the right one succeeds.
    let e = objects::delete(p, me, cal, "a.ics", Precondition::IfMatch("\"1\"".into())).await;
    assert!(matches!(e, Err(DomainError::PreconditionFailed)));
    objects::delete(p, me, cal, "a.ics", Precondition::IfMatch(w.meta.etag())).await.unwrap();
    assert!(matches!(objects::get(p, me, cal, "a.ics").await, Err(DomainError::NotFound)));
    db.drop().await;
}

#[tokio::test]
async fn objects_are_validated_and_uids_unique() {
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "me", "").await.unwrap();
    let cal = calendar(p, me, "family").await;
    let book = addressbook(p, me).await;
    objects::put(p, me, cal, "a.ics", &event("same", "20260601T090000Z", ""), Precondition::None).await.unwrap();

    let e = objects::put(p, me, cal, "b.ics", &event("same", "20260602T090000Z", ""), Precondition::None).await;
    assert!(matches!(e, Err(DomainError::UidConflict { existing }) if existing == "a.ics"));
    let card = "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:c\r\nFN:Synthetic\r\nEND:VCARD\r\n";
    assert!(matches!(objects::put(p, me, cal, "c.vcf", card, Precondition::None).await, Err(DomainError::WrongKind)));
    let ev = event("x", "20260601T090000Z", "");
    assert!(matches!(objects::put(p, me, book, "x.ics", &ev, Precondition::None).await, Err(DomainError::WrongKind)));
    let journal = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VJOURNAL\r\nUID:j\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n";
    let e = objects::put(p, me, cal, "j.ics", journal, Precondition::None).await;
    assert!(matches!(e, Err(DomainError::ComponentNotAccepted)));
    let e = objects::put(p, me, cal, "bad.ics", "BEGIN:VCALENDAR\r\n", Precondition::None).await;
    assert!(matches!(e, Err(DomainError::Invalid(_))));
    let e = objects::put(p, me, cal, "../x.ics", &ev, Precondition::None).await;
    assert!(matches!(e, Err(DomainError::BadName)));
    // Cards without UID are keyed by name, so two of them do not clash.
    let no_uid = "BEGIN:VCARD\r\nVERSION:3.0\r\nFN:Synthetic\r\nEND:VCARD\r\n";
    objects::put(p, me, book, "1.vcf", no_uid, Precondition::None).await.unwrap();
    objects::put(p, me, book, "2.vcf", no_uid, Precondition::None).await.unwrap();
    db.drop().await;
}

#[tokio::test]
async fn alarms_are_personal() {
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "me", "").await.unwrap();
    let partner = principals::create_user(p, "partner", "").await.unwrap();
    let cal = calendar(p, me, "family").await;
    collections::set_grant(p, me, cal, partner, Some(Access::Write)).await.unwrap();

    let mine = event("a", "20260601T090000Z", ALARM);
    let w = objects::put(p, me, cal, "a.ics", &mine, Precondition::None).await.unwrap();
    assert_eq!(objects::get(p, me, cal, "a.ics").await.unwrap().body, mine, "I get my alarm back, byte for byte");
    let theirs = objects::get(p, partner, cal, "a.ics").await.unwrap().body;
    assert!(!theirs.contains("VALARM"), "my alarm never reaches my partner");
    let stored: String = sqlx::query_scalar("SELECT body FROM objects").fetch_one(p).await.unwrap();
    assert!(!stored.contains("VALARM"), "the shared copy holds no alarms");

    // The partner edits the event and adds their own alarm.
    let their_alarm = ALARM.replace("-PT15M", "-PT1H");
    let edited = event("a", "20260601T100000Z", &their_alarm);
    objects::put(p, partner, cal, "a.ics", &edited, Precondition::IfMatch(w.meta.etag())).await.unwrap();
    let mine_now = objects::get(p, me, cal, "a.ics").await.unwrap().body;
    assert!(mine_now.contains("DTSTART:20260601T100000Z"), "I see their edit");
    assert!(mine_now.contains("TRIGGER:-PT15M") && !mine_now.contains("TRIGGER:-PT1H"), "with my alarm, not theirs");
    let theirs_now = objects::get(p, partner, cal, "a.ics").await.unwrap().body;
    assert!(theirs_now.contains("TRIGGER:-PT1H") && !theirs_now.contains("TRIGGER:-PT15M"));

    // I remove my alarm: only mine goes.
    objects::put(p, me, cal, "a.ics", &event("a", "20260601T100000Z", ""), Precondition::None).await.unwrap();
    assert!(!objects::get(p, me, cal, "a.ics").await.unwrap().body.contains("VALARM"));
    assert!(objects::get(p, partner, cal, "a.ics").await.unwrap().body.contains("TRIGGER:-PT1H"));
    db.drop().await;
}

#[tokio::test]
async fn access_is_404_for_the_unseen_and_read_only_for_readers() {
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "me", "").await.unwrap();
    let stranger = principals::create_user(p, "stranger", "").await.unwrap();
    let reader = principals::create_user(p, "reader", "").await.unwrap();
    let kid = principals::create_user(p, "kid", "").await.unwrap();
    let family = principals::create_group(p, "family", "Family").await.unwrap();
    principals::add_member(p, family, kid).await.unwrap();
    let cal = calendar(p, me, "family").await;
    objects::put(p, me, cal, "a.ics", &event("a", "20260601T090000Z", ""), Precondition::None).await.unwrap();
    collections::set_grant(p, me, cal, reader, Some(Access::Read)).await.unwrap();
    collections::set_grant(p, me, cal, family, Some(Access::Write)).await.unwrap();

    // The stranger learns nothing: the same NotFound as for a missing collection.
    assert!(matches!(collections::get(p, stranger, cal).await, Err(DomainError::NotFound)));
    assert!(matches!(collections::by_path(p, stranger, "me", "family").await, Err(DomainError::NotFound)));
    assert!(matches!(objects::get(p, stranger, cal, "a.ics").await, Err(DomainError::NotFound)));
    let e = objects::put(p, stranger, cal, "b.ics", &event("b", "20260601T090000Z", ""), Precondition::None).await;
    assert!(matches!(e, Err(DomainError::NotFound)));
    assert!(matches!(objects::delete(p, stranger, cal, "a.ics", Precondition::None).await, Err(DomainError::NotFound)));
    assert!(collections::visible(p, stranger, CollectionKind::Calendar).await.unwrap().is_empty());

    // The reader reads but cannot write or delete.
    assert_eq!(collections::get(p, reader, cal).await.unwrap().access, Access::Read);
    objects::get(p, reader, cal, "a.ics").await.unwrap();
    let e = objects::put(p, reader, cal, "b.ics", &event("b", "20260601T090000Z", ""), Precondition::None).await;
    assert!(matches!(e, Err(DomainError::ReadOnly)));
    assert!(matches!(objects::delete(p, reader, cal, "a.ics", Precondition::None).await, Err(DomainError::ReadOnly)));
    assert!(matches!(collections::delete(p, reader, cal).await, Err(DomainError::ReadOnly)));

    // A group grant reaches its members.
    assert_eq!(collections::get(p, kid, cal).await.unwrap().access, Access::Write);
    objects::put(p, kid, cal, "k.ics", &event("k", "20260601T090000Z", ""), Precondition::None).await.unwrap();
    assert_eq!(collections::visible(p, kid, CollectionKind::Calendar).await.unwrap().len(), 1);

    // Disabling an account cuts it off.
    principals::set_disabled(p, kid, true).await.unwrap();
    assert!(matches!(collections::get(p, kid, cal).await, Err(DomainError::NotFound)));
    // Revoking a grant does too.
    collections::set_grant(p, me, cal, reader, None).await.unwrap();
    assert!(matches!(objects::get(p, reader, cal, "a.ics").await, Err(DomainError::NotFound)));
    db.drop().await;
}

#[tokio::test]
async fn the_change_log_reports_changes_and_tombstones() {
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "me", "").await.unwrap();
    let cal = calendar(p, me, "family").await;
    for n in ["a", "b"] {
        objects::put(p, me, cal, &format!("{n}.ics"), &event(n, "20260601T090000Z", ""), Precondition::None)
            .await
            .unwrap();
    }
    let full = objects::changes_since(p, me, cal, None).await.unwrap();
    assert_eq!(full.token, 2);
    assert_eq!(full.changed.iter().map(|m| m.name.as_str()).collect::<Vec<_>>(), ["a.ics", "b.ics"]);

    objects::put(p, me, cal, "a.ics", &event("a", "20260602T090000Z", ""), Precondition::None).await.unwrap();
    objects::delete(p, me, cal, "b.ics", Precondition::None).await.unwrap();
    objects::put(p, me, cal, "c.ics", &event("c", "20260601T090000Z", ""), Precondition::None).await.unwrap();
    let delta = objects::changes_since(p, me, cal, Some(full.token)).await.unwrap();
    assert_eq!(delta.token, 5);
    assert_eq!(
        delta.changed.iter().map(|m| (m.name.as_str(), m.revision)).collect::<Vec<_>>(),
        [("a.ics", 3), ("c.ics", 5)]
    );
    assert_eq!(delta.deleted, ["b.ics"]);

    // Nothing new: an empty answer and the same token.
    let none = objects::changes_since(p, me, cal, Some(delta.token)).await.unwrap();
    assert!(none.changed.is_empty() && none.deleted.is_empty());
    assert_eq!(none.token, 5);
    // A deleted then recreated name counts as changed, not deleted.
    objects::delete(p, me, cal, "c.ics", Precondition::None).await.unwrap();
    objects::put(p, me, cal, "c.ics", &event("c", "20260601T090000Z", ""), Precondition::None).await.unwrap();
    let again = objects::changes_since(p, me, cal, Some(5)).await.unwrap();
    assert_eq!(again.changed.len(), 1);
    assert!(again.deleted.is_empty());
    // Tokens from the future are refused.
    assert!(matches!(objects::changes_since(p, me, cal, Some(999)).await, Err(DomainError::BadSyncToken)));
    // The collection's sequence is the token.
    assert_eq!(collections::get(p, me, cal).await.unwrap().sequence, again.token);
    db.drop().await;
}

#[tokio::test]
async fn range_candidates_never_miss() {
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "me", "").await.unwrap();
    let cal = calendar(p, me, "family").await;
    let put = |name: &'static str, body: String| async move {
        objects::put(p, me, cal, name, &body, Precondition::None).await.unwrap();
    };
    put("june.ics", event("june", "20260601T090000Z", "")).await;
    put("weekly.ics", event("weekly", "20200106T090000Z", "RRULE:FREQ=WEEKLY\r\n")).await;
    put("floating.ics", event("floating", "20260701T090000", "")).await;
    put(
        "todo.ics",
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VTODO\r\nUID:todo\r\nSUMMARY:Undated\r\nEND:VTODO\r\nEND:VCALENDAR\r\n".into(),
    )
    .await;
    let names = |v: Vec<calzone_server::domain::ObjectMeta>| v.into_iter().map(|m| m.name).collect::<Vec<_>>();

    let july = objects::in_range(p, me, cal, dt("2026-07-01 00:00"), dt("2026-07-02 00:00")).await.unwrap();
    assert_eq!(names(july), ["floating.ics", "todo.ics", "weekly.ics"]);
    let june1 = objects::in_range(p, me, cal, dt("2026-06-01 09:30"), dt("2026-06-01 09:45")).await.unwrap();
    assert_eq!(names(june1), ["june.ics", "todo.ics", "weekly.ics"]);
    let before = objects::in_range(p, me, cal, dt("2019-01-01 00:00"), dt("2019-02-01 00:00")).await.unwrap();
    assert_eq!(names(before), ["todo.ics"]);
    db.drop().await;
}

#[tokio::test]
async fn concurrent_creates_of_one_name_let_exactly_one_win() {
    let Some(db) = common::Db::new().await else { return };
    let p = db.pool.clone();
    let me = principals::create_user(&p, "me", "").await.unwrap();
    let cal = calendar(&p, me, "family").await;
    let mut tasks = Vec::new();
    for i in 0..8 {
        let p = p.clone();
        tasks.push(tokio::spawn(async move {
            let body = event("race", &format!("2026060{}T090000Z", i % 9 + 1), "");
            objects::put(&p, me, cal, "race.ics", &body, Precondition::IfNoneMatchAny).await
        }));
    }
    let mut won = 0;
    for t in tasks {
        match t.await.unwrap() {
            Ok(_) => won += 1,
            Err(DomainError::PreconditionFailed) => {}
            Err(e) => panic!("unexpected {e}"),
        }
    }
    assert_eq!(won, 1);
    // Concurrent writes to different names all land, each with its own sequence.
    let mut tasks = Vec::new();
    for i in 0..8 {
        let p = p.clone();
        tasks.push(tokio::spawn(async move {
            objects::put(
                &p,
                me,
                cal,
                &format!("n{i}.ics"),
                &event(&format!("n{i}"), "20260601T090000Z", ""),
                Precondition::None,
            )
            .await
            .unwrap()
            .meta
            .revision
        }));
    }
    let mut revs = Vec::new();
    for t in tasks {
        revs.push(t.await.unwrap());
    }
    revs.sort_unstable();
    assert_eq!(revs, (2..=9).collect::<Vec<i64>>());
    let changes: i64 = sqlx::query_scalar("SELECT count(*) FROM changes").fetch_one(&p).await.unwrap();
    assert_eq!(changes, 9);
    db.drop().await;
}

#[tokio::test]
async fn collections_names_and_properties() {
    let Some(db) = common::Db::new().await else { return };
    let p = &db.pool;
    let me = principals::create_user(p, "me", "").await.unwrap();
    assert!(matches!(principals::create_user(p, "ME", "").await, Err(DomainError::NameTaken)));
    assert!(matches!(principals::create_user(p, "bad name", "").await, Err(DomainError::BadName)));
    let cal = calendar(p, me, "family").await;
    let dup = collections::create(
        p,
        me,
        NewCollection {
            kind: CollectionKind::Calendar,
            slug: "family",
            display_name: "",
            description: "",
            color: None,
            components: &[],
        },
    )
    .await;
    assert!(matches!(dup, Err(DomainError::NameTaken)));
    let c = collections::update_properties(p, me, cal, Some("Gezin"), None, Some(None)).await.unwrap();
    assert_eq!(c.display_name, "Gezin");
    assert_eq!(c.color, None);
    assert_eq!(c.owner_name, "me");
    assert_eq!(collections::by_path(p, me, "ME", "family").await.unwrap().id, cal);
    collections::delete(p, me, cal).await.unwrap();
    assert!(matches!(collections::get(p, me, cal).await, Err(DomainError::NotFound)));
    db.drop().await;
}
