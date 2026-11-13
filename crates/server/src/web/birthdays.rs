//! Birthdays (decision 57): the page that turns it on and sets it up, and
//! the page that finds the birthday events people made by hand and
//! removes them (copying a date to its contact first when it has none),
//! as one change with one Undo.

use askama::Template;
use axum::extract::{Path, RawForm, RawQuery, State};
use axum::response::{IntoResponse, Redirect};
use chrono::{Datelike, NaiveDate};
use uuid::Uuid;

use super::session::WebUser;
use super::{Nav, Page, WebError, html, i18n};
use crate::AppState;
use crate::domain::DomainError;
use crate::domain::birthdays::{self, Found, REMINDERS, Removal};
use crate::domain::events::{self, Target};
use calzone_core::birthdays::Day;
use calzone_core::quickadd::Lang;

fn pairs_of(s: &str) -> Vec<(String, String)> {
    serde_urlencoded::from_str(s).unwrap_or_default()
}

fn one<'a>(pairs: &'a [(String, String)], k: &str) -> &'a str {
    pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.as_str()).unwrap_or("")
}

pub struct BookRow {
    pub id: Uuid,
    pub name: String,
    pub whose: String,
    pub included: bool,
}

#[derive(Template)]
#[template(path = "birthdays.html")]
struct BirthdaysPage {
    nav: Nav,
    b: &'static i18n::B,
    on: bool,
    books: Vec<BookRow>,
    anniversaries: bool,
    reminder: String,
    reminders: Vec<(String, &'static str)>,
    manual: usize,
    manual_text: String,
    saved: bool,
}

/// `GET /birthdays`.
pub async fn page(State(st): State<AppState>, user: WebUser, RawQuery(q): RawQuery) -> Page {
    let b = i18n::b(user.lang);
    let pairs = pairs_of(q.as_deref().unwrap_or(""));
    let settings = birthdays::of(&st.pool, user.id()).await?;
    let books = birthdays::books(&st.pool, user.id())
        .await?
        .into_iter()
        .map(|x| BookRow {
            id: x.id,
            whose: if x.own {
                b.book_own.to_string()
            } else if x.birthdays_only {
                format!("{} {}", b.book_birthdays_only, x.owner)
            } else {
                format!("{} {}", b.book_shared, x.owner)
            },
            name: x.name,
            included: x.included,
        })
        .collect();
    let manual = if settings.is_some() { birthdays::manual(&st.pool, user.id()).await?.len() } else { 0 };
    let manual_text = match manual {
        0 => b.cleanup_none.to_string(),
        1 => b.cleanup_one.to_string(),
        n => i18n::n(b.cleanup_some, n),
    };
    html(BirthdaysPage {
        nav: Nav::new(&st, &user, "calendar"),
        b,
        on: settings.is_some(),
        books,
        anniversaries: settings.as_ref().is_some_and(|s| s.anniversaries),
        reminder: settings.as_ref().and_then(|s| s.reminder).map(|m| m.to_string()).unwrap_or_else(|| "none".into()),
        reminders: REMINDERS
            .iter()
            .zip([b.rem_day, b.rem_before, b.rem_week])
            .map(|(m, l)| (m.to_string(), l))
            .collect(),
        manual,
        manual_text,
        saved: one(&pairs, "saved") == "1",
    })
}

/// `POST /birthdays/on`.
pub async fn turn_on(State(st): State<AppState>, user: WebUser) -> Page {
    birthdays::enable(&st.pool, user.id(), i18n::b(user.lang).title, i18n::code(user.lang)).await?;
    Ok(Redirect::to("/birthdays").into_response())
}

/// `POST /birthdays/off`: only with the box ticked.
pub async fn turn_off(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let pairs = pairs_of(std::str::from_utf8(&body).unwrap_or(""));
    if one(&pairs, "confirm") != "yes" {
        return Err(WebError::bad(i18n::c(user.lang).need_confirm));
    }
    birthdays::disable(&st.pool, user.id()).await?;
    Ok(Redirect::to("/birthdays").into_response())
}

/// `POST /birthdays`: the options and which books.
pub async fn save(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let pairs = pairs_of(std::str::from_utf8(&body).unwrap_or(""));
    let reminder = match one(&pairs, "reminder") {
        "" | "none" => None,
        v => Some(v.parse::<i32>().map_err(|_| WebError::bad(i18n::a(user.lang).err_bad_form))?),
    };
    let books: Vec<Uuid> = pairs.iter().filter(|(k, _)| k == "book").filter_map(|(_, v)| v.parse().ok()).collect();
    birthdays::set_options(&st.pool, user.id(), one(&pairs, "anniversaries") == "on", reminder, &books).await?;
    Ok(Redirect::to("/birthdays?saved=1").into_response())
}

pub struct ManualRow {
    pub key: String,
    pub etag: String,
    pub title: String,
    pub when: String,
    pub calendar: String,
    pub shared: bool,
    /// Ticked when the page opens; not offered at all for `Differs`.
    pub pick: bool,
    pub covered: Option<String>,
    pub copy: Option<CopyRow>,
    pub differs: Option<(String, String)>,
}

pub struct CopyRow {
    pub contact: String,
    pub card: String,
    pub etag: String,
    pub date: String,
}

#[derive(Template)]
#[template(path = "birthdays_cleanup.html")]
struct CleanupPage {
    nav: Nav,
    b: &'static i18n::B,
    rows: Vec<ManualRow>,
    pickable: usize,
    notice: Option<String>,
    undo: Option<Uuid>,
    warn: bool,
}

fn day_text(lang: Lang, d: Day) -> String {
    let months = match lang {
        Lang::Nl => ["jan", "feb", "mrt", "apr", "mei", "jun", "jul", "aug", "sep", "okt", "nov", "dec"],
        Lang::En => ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"],
    };
    let m = months[(d.month as usize).clamp(1, 12) - 1];
    let md = match lang {
        Lang::Nl => format!("{} {m}", d.day),
        Lang::En => format!("{m} {}", d.day),
    };
    match d.year {
        Some(y) => format!("{md} {y}"),
        None => md,
    }
}

/// `GET /birthdays/cleanup`.
pub async fn cleanup(State(st): State<AppState>, user: WebUser, RawQuery(q): RawQuery) -> Page {
    let b = i18n::b(user.lang);
    if birthdays::of(&st.pool, user.id()).await?.is_none() {
        return Ok(Redirect::to("/birthdays").into_response());
    }
    let pairs = pairs_of(q.as_deref().unwrap_or(""));
    let count = |k: &str| one(&pairs, k).parse::<usize>().unwrap_or(0);
    let undo = one(&pairs, "change").parse::<Uuid>().ok();
    let (notice, warn) = if one(&pairs, "stale") == "1" {
        (Some(b.cl_stale.to_string()), true)
    } else if one(&pairs, "undone") == "1" {
        (Some(b.cl_undone.to_string()), false)
    } else if undo.is_some() {
        let mut s = match count("n") {
            1 => b.cl_done_one.to_string(),
            n => i18n::n(b.cl_done, n),
        };
        match count("copied") {
            0 => {}
            1 => s = format!("{s} {}", b.cl_copied_one),
            n => s = format!("{s} {}", i18n::n(b.cl_copied, n)),
        }
        (Some(s), false)
    } else {
        (None, false)
    };
    let rows: Vec<ManualRow> = birthdays::manual(&st.pool, user.id())
        .await?
        .into_iter()
        .map(|m| {
            let when = day_text(user.lang, Day { month: m.start.month(), day: m.start.day(), year: None });
            let mut row = ManualRow {
                key: format!("{}/{}", m.collection, m.name),
                etag: m.etag,
                title: m.title,
                when,
                calendar: m.calendar,
                shared: m.shared,
                pick: true,
                covered: None,
                copy: None,
                differs: None,
            };
            match m.found {
                Found::Covered { contact } => row.covered = Some(contact),
                Found::Copy { contact, book, name, etag } => {
                    row.copy = Some(CopyRow {
                        contact,
                        card: format!("{book}/{name}"),
                        etag,
                        date: m.start.format("%Y-%m-%d").to_string(),
                    })
                }
                Found::Differs { contact, birthday } => {
                    row.pick = false;
                    row.differs = Some((contact, day_text(user.lang, birthday)));
                }
            }
            row
        })
        .collect();
    html(CleanupPage {
        nav: Nav::new(&st, &user, "calendar"),
        b,
        pickable: rows.iter().filter(|r| r.pick).count(),
        rows,
        notice,
        undo: undo.filter(|_| !warn),
        warn,
    })
}

fn split_key(s: &str) -> Option<(Uuid, String)> {
    let (c, n) = s.split_once('/')?;
    Some((c.parse().ok()?, n.to_string())).filter(|(_, n)| !n.is_empty())
}

/// `POST /birthdays/cleanup`: the ticked events go, with dates copied to
/// contacts first, in one change. Only with the box ticked.
pub async fn remove(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let b = i18n::b(user.lang);
    let pairs = pairs_of(std::str::from_utf8(&body).unwrap_or(""));
    if one(&pairs, "confirm") != "yes" {
        return Err(WebError::bad(i18n::c(user.lang).need_confirm));
    }
    let picks: Vec<&str> = pairs.iter().filter(|(k, _)| k == "pick").map(|(_, v)| v.as_str()).collect();
    if picks.is_empty() {
        return Err(WebError::bad(b.cl_pick_one));
    }
    let bad = || WebError::bad(i18n::a(user.lang).err_bad_form);
    let mut removals = Vec::with_capacity(picks.len());
    for key in picks {
        let (collection, name) = split_key(key).ok_or_else(bad)?;
        let event = Target { collection, name, etag: one(&pairs, &format!("etag:{key}")).to_string() };
        let card = one(&pairs, &format!("card:{key}"));
        let copy = if card.is_empty() {
            None
        } else {
            let (bc, bn) = split_key(card).ok_or_else(bad)?;
            let date = NaiveDate::parse_from_str(one(&pairs, &format!("date:{key}")), "%Y-%m-%d")
                .map_err(|_| WebError::bad(b.cl_bad_date))?;
            let year = (one(&pairs, &format!("noyear:{key}")) != "on").then_some(date.year());
            let day = Day { month: date.month(), day: date.day(), year };
            Some((Target { collection: bc, name: bn, etag: one(&pairs, &format!("cardetag:{key}")).to_string() }, day))
        };
        removals.push(Removal { event, copy });
    }
    let copied = removals.iter().filter(|r| r.copy.is_some()).count();
    let now = chrono::Utc::now().naive_utc();
    match birthdays::remove_manual(&st.pool, user.id(), &removals, now).await {
        Ok(change) => {
            Ok(Redirect::to(&format!("/birthdays/cleanup?change={change}&n={}&copied={copied}", removals.len()))
                .into_response())
        }
        Err(DomainError::PreconditionFailed | DomainError::NotFound) => {
            Ok(Redirect::to("/birthdays/cleanup?stale=1").into_response())
        }
        Err(e) => Err(e.into()),
    }
}

/// `POST /birthdays/undo/{change}`.
pub async fn undo(State(st): State<AppState>, user: WebUser, Path(change): Path<Uuid>) -> Page {
    match events::undo(&st.pool, user.id(), change).await {
        Ok(()) => {
            if let Some(s) = birthdays::of(&st.pool, user.id()).await? {
                birthdays::rebuild(&st.pool, s.collection).await?;
            }
            Ok(Redirect::to("/birthdays/cleanup?undone=1").into_response())
        }
        Err(DomainError::PreconditionFailed) => Err(WebError::bad(i18n::t(user.lang).msg_cannot_undo)),
        Err(e) => Err(e.into()),
    }
}

/// `POST /collections/{id}/birthdays/share`: an address book's birthdays
/// only, to a person or group.
pub async fn share(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>, RawForm(body): RawForm) -> Page {
    let pairs = pairs_of(std::str::from_utf8(&body).unwrap_or(""));
    let principal: Uuid =
        one(&pairs, "principal").parse().map_err(|_| WebError::bad(i18n::a(user.lang).err_bad_form))?;
    crate::domain::accounts::get(&st.pool, principal).await?;
    birthdays::set_share(&st.pool, user.id(), id, principal, true).await?;
    Ok(Redirect::to(&format!("/collections/{id}")).into_response())
}

/// `POST /collections/{id}/birthdays/unshare/{principal}`.
pub async fn unshare(State(st): State<AppState>, user: WebUser, Path((id, principal)): Path<(Uuid, Uuid)>) -> Page {
    birthdays::set_share(&st.pool, user.id(), id, principal, false).await?;
    Ok(Redirect::to(&format!("/collections/{id}")).into_response())
}
