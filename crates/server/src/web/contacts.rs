//! Contacts pages (phase 2 step 4): a searchable list, one contact, and
//! the edit form. Plain forms with post-redirect-get (they work without
//! JavaScript; HTMX boosts them), the search live through `hx-get`.
//! Deleting and saving show a notice with Undo instead of a confirm
//! dialog (decision 12).

use askama::Template;
use axum::extract::{Path, Query, RawForm, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG};
use axum::http::{HeaderMap, HeaderValue};
use axum::response::{IntoResponse, Redirect};
use calzone_core::card::{self, Address, Birthday, CardChange, Kind, Typed};
use chrono::NaiveDate;
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde::Deserialize;
use uuid::Uuid;

use super::session::WebUser;
use super::{Nav, Page, WebError, html, i18n};
use crate::AppState;
use crate::domain::contacts::{self, ContactDetail};
use crate::domain::{DomainError, events};

/// Characters a path segment must not carry raw.
const SEGMENT: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'/')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'`')
    .add(b'{')
    .add(b'}');

fn href(collection: Uuid, name: &str) -> String {
    format!("/contacts/{collection}/{}", utf8_percent_encode(name, SEGMENT))
}

const PALETTE: [&str; 8] = ["#3a7bd5", "#c2185b", "#00897b", "#f4511e", "#7e57c2", "#43a047", "#8d6e63", "#1e88e5"];

/// A stable color for someone without a photo.
fn color_for(name: &str) -> &'static str {
    let h = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b));
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).collect();
    let pick = |w: &str| w.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [one] => one.chars().take(2).collect::<String>().to_uppercase(),
        [first, .., last] => pick(first) + &pick(last),
    }
}

/// A list letter: the first letter, `#` for anything else.
fn letter(name: &str) -> String {
    match name.trim().chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect::<String>().chars().take(1).collect(),
        _ => "#".into(),
    }
}

pub struct RowView {
    pub href: String,
    pub display: String,
    pub sub: String,
    pub initials: String,
    pub color: &'static str,
    pub photo: Option<String>,
}

pub struct Group {
    pub letter: String,
    pub rows: Vec<RowView>,
}

pub struct BookView {
    pub name: String,
    pub owner: String,
    pub own: bool,
    pub count: usize,
    pub groups: Vec<Group>,
}

/// A notice from the redirect after a write, with an Undo when it applies.
pub struct Notice {
    pub text: &'static str,
    pub undo: Option<Uuid>,
    pub warn: bool,
}

#[derive(Deserialize, Default)]
pub struct Flash {
    #[serde(default)]
    q: String,
    saved: Option<Uuid>,
    added: Option<Uuid>,
    deleted: Option<Uuid>,
    undone: Option<String>,
    stale: Option<String>,
}

fn notice(t: &'static i18n::T, f: &Flash) -> Option<Notice> {
    if let Some(id) = f.deleted {
        return Some(Notice { text: t.contact_deleted, undo: Some(id), warn: false });
    }
    if let Some(id) = f.saved {
        return Some(Notice { text: t.contact_saved, undo: Some(id), warn: false });
    }
    if let Some(id) = f.added {
        return Some(Notice { text: t.contact_added, undo: Some(id), warn: false });
    }
    if f.undone.is_some() {
        return Some(Notice { text: t.undone, undo: None, warn: false });
    }
    if f.stale.is_some() {
        return Some(Notice { text: t.stale_contact, undo: None, warn: true });
    }
    None
}

#[derive(Template)]
#[template(path = "contacts.html")]
struct ListPage {
    nav: Nav,
    q: String,
    books: Vec<BookView>,
    total: usize,
    can_add: bool,
    notice: Option<Notice>,
}

pub async fn list(State(st): State<AppState>, user: WebUser, Query(f): Query<Flash>) -> Page {
    let t = i18n::t(user.lang);
    let books = contacts::list(&st.pool, user.id(), &f.q).await?;
    let can_add = books.iter().any(|b| b.writable);
    let mut total = 0;
    let views = books
        .into_iter()
        .map(|b| {
            let count = b.contacts.len();
            total += count;
            let mut groups: Vec<Group> = Vec::new();
            for r in b.contacts {
                let l = letter(&r.display);
                let sub = [r.org.clone(), r.phone.clone().unwrap_or_default(), r.email.clone().unwrap_or_default()]
                    .into_iter()
                    .find(|s| !s.is_empty() && *s != r.display)
                    .unwrap_or_default();
                let row = RowView {
                    href: href(r.collection, &r.name),
                    initials: initials(&r.display),
                    color: color_for(&r.display),
                    photo: r
                        .has_photo
                        .then(|| format!("/contacts/photo/{}/{}", r.collection, utf8_percent_encode(&r.name, SEGMENT))),
                    display: r.display,
                    sub,
                };
                match groups.last_mut() {
                    Some(g) if g.letter == l => g.rows.push(row),
                    _ => groups.push(Group { letter: l, rows: vec![row] }),
                }
            }
            BookView { name: b.name, owner: b.owner, own: b.own, count, groups }
        })
        .collect();
    html(ListPage {
        nav: Nav::new(&st, &user, "contacts"),
        q: f.q.clone(),
        books: views,
        total,
        can_add,
        notice: notice(t, &f),
    })
}

pub struct TypedView {
    pub kind: &'static str,
    pub value: String,
    pub link: String,
}

pub struct AddressView {
    pub kind: &'static str,
    pub text: String,
    pub map: String,
}

#[derive(Template)]
#[template(path = "contact.html")]
struct ContactPage {
    nav: Nav,
    d: ContactDetail,
    href: String,
    initials: String,
    color: &'static str,
    photo: Option<String>,
    phones: Vec<TypedView>,
    emails: Vec<TypedView>,
    addresses: Vec<AddressView>,
    birthday: Option<String>,
    notice: Option<Notice>,
}

fn kind_word(t: &'static i18n::T, k: Kind) -> &'static str {
    match k {
        Kind::Mobile => t.kind_mobile,
        Kind::Home => t.kind_home,
        Kind::Work => t.kind_work,
        Kind::Other => t.kind_other,
    }
}

fn month_name(lang: calzone_core::quickadd::Lang, m: u32) -> &'static str {
    const EN: [&str; 12] = [
        "January",
        "February",
        "March",
        "April",
        "May",
        "June",
        "July",
        "August",
        "September",
        "October",
        "November",
        "December",
    ];
    const NL: [&str; 12] = [
        "januari",
        "februari",
        "maart",
        "april",
        "mei",
        "juni",
        "juli",
        "augustus",
        "september",
        "oktober",
        "november",
        "december",
    ];
    let i = (m.clamp(1, 12) - 1) as usize;
    match lang {
        calzone_core::quickadd::Lang::En => EN[i],
        calzone_core::quickadd::Lang::Nl => NL[i],
    }
}

/// `10 December 1815` (`10 december 1815` in Dutch), or without the year.
fn birthday_text(lang: calzone_core::quickadd::Lang, b: Birthday) -> String {
    let (d, m, y) = card::birthday_parts(b);
    match y {
        Some(y) => format!("{d} {} {y}", month_name(lang, m)),
        None => format!("{d} {}", month_name(lang, m)),
    }
}

fn url_query(s: &str) -> String {
    utf8_percent_encode(s, percent_encoding::NON_ALPHANUMERIC).to_string()
}

pub async fn show(
    State(st): State<AppState>,
    user: WebUser,
    Path((c, n)): Path<(Uuid, String)>,
    Query(f): Query<Flash>,
) -> Page {
    let t = i18n::t(user.lang);
    let d = contacts::detail(&st.pool, user.id(), c, &n).await?;
    let shown = if d.card.name.trim().is_empty() { d.card.org.clone() } else { d.card.name.clone() };
    let phones = d
        .card
        .phones
        .iter()
        .map(|p| TypedView {
            kind: kind_word(t, p.kind),
            link: format!("tel:{}", p.value.chars().filter(|c| c.is_ascii_digit() || *c == '+').collect::<String>()),
            value: p.value.clone(),
        })
        .collect();
    let emails = d
        .card
        .emails
        .iter()
        .map(|e| TypedView { kind: kind_word(t, e.kind), link: format!("mailto:{}", e.value), value: e.value.clone() })
        .collect();
    let addresses = d
        .card
        .addresses
        .iter()
        .map(|a| AddressView {
            kind: kind_word(t, a.kind.unwrap_or(Kind::Other)),
            map: format!("https://www.openstreetmap.org/search?query={}", url_query(&a.one_line())),
            text: a.one_line(),
        })
        .collect();
    let photo = matches!(d.card.photo, Some(card::Photo::Inline { .. }))
        .then(|| format!("/contacts/photo/{c}/{}", utf8_percent_encode(&n, SEGMENT)));
    html(ContactPage {
        nav: Nav::new(&st, &user, "contacts"),
        href: href(c, &n),
        initials: initials(&shown),
        color: color_for(&shown),
        photo,
        phones,
        emails,
        addresses,
        birthday: d.card.birthday.map(|b| birthday_text(user.lang, b)),
        notice: notice(t, &f),
        d,
    })
}

/// `GET /contacts/photo/{c}/{n}`: an inline photo, never a fetched link.
pub async fn photo(
    State(st): State<AppState>,
    user: WebUser,
    headers: HeaderMap,
    Path((c, n)): Path<(Uuid, String)>,
) -> Page {
    let (mime, bytes, etag) = contacts::photo(&st.pool, user.id(), c, &n).await?;
    let etag_v = HeaderValue::from_str(&etag).unwrap_or(HeaderValue::from_static("\"0\""));
    let common = [(CACHE_CONTROL, HeaderValue::from_static("private, no-cache")), (ETAG, etag_v)];
    if headers.get(axum::http::header::IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == etag.as_bytes()) {
        return Ok((axum::http::StatusCode::NOT_MODIFIED, common).into_response());
    }
    let ct = HeaderValue::from_str(&mime).unwrap_or(HeaderValue::from_static("application/octet-stream"));
    Ok((common, [(CONTENT_TYPE, ct)], bytes).into_response())
}

pub struct TypedRow {
    pub kind: &'static str,
    pub value: String,
}

pub struct AddressRow {
    pub kind: &'static str,
    pub street: String,
    pub postcode: String,
    pub city: String,
    pub region: String,
    pub country: String,
}

#[derive(Template)]
#[template(path = "contact_form.html")]
struct FormPage {
    nav: Nav,
    new: bool,
    action: String,
    back: String,
    etag: String,
    books: Vec<(Uuid, String)>,
    given: String,
    family: String,
    org: String,
    phones: Vec<TypedRow>,
    emails: Vec<TypedRow>,
    addresses: Vec<AddressRow>,
    birthday: String,
    note: String,
    title: String,
}

fn rows_from(list: &[Typed], extra: usize) -> Vec<TypedRow> {
    let mut v: Vec<TypedRow> =
        list.iter().map(|t| TypedRow { kind: t.kind.as_str(), value: t.value.clone() }).collect();
    v.extend((0..extra).map(|_| TypedRow { kind: "mobile", value: String::new() }));
    v
}

fn address_rows(list: &[Address]) -> Vec<AddressRow> {
    let mut v: Vec<AddressRow> = list
        .iter()
        .map(|a| AddressRow {
            kind: a.kind.unwrap_or(Kind::Other).as_str(),
            street: a.street.clone(),
            postcode: a.postcode.clone(),
            city: a.city.clone(),
            region: a.region.clone(),
            country: a.country.clone(),
        })
        .collect();
    v.push(AddressRow {
        kind: "home",
        street: String::new(),
        postcode: String::new(),
        city: String::new(),
        region: String::new(),
        country: String::new(),
    });
    v
}

fn birthday_field(b: Option<Birthday>) -> String {
    match b {
        Some(Birthday::Full(d)) => d.format("%Y-%m-%d").to_string(),
        _ => String::new(),
    }
}

#[derive(Deserialize)]
pub struct NewQuery {
    book: Option<Uuid>,
}

pub async fn new_form(State(st): State<AppState>, user: WebUser, Query(q): Query<NewQuery>) -> Page {
    let t = i18n::t(user.lang);
    let mut books = contacts::writable_books(&st.pool, user.id()).await?;
    if books.is_empty() {
        return Err(WebError::bad(t.no_book));
    }
    if let Some(b) = q.book
        && let Some(i) = books.iter().position(|x| x.0 == b)
    {
        let chosen = books.remove(i);
        books.insert(0, chosen);
    }
    html(FormPage {
        nav: Nav::new(&st, &user, "contacts"),
        new: true,
        action: "/contacts/new".into(),
        back: "/contacts".into(),
        etag: String::new(),
        books,
        given: String::new(),
        family: String::new(),
        org: String::new(),
        phones: rows_from(&[], 2),
        emails: rows_from(&[], 1),
        addresses: address_rows(&[]),
        birthday: String::new(),
        note: String::new(),
        title: t.new_contact.into(),
    })
}

pub async fn edit_form(State(st): State<AppState>, user: WebUser, Path((c, n)): Path<(Uuid, String)>) -> Page {
    let d = contacts::detail(&st.pool, user.id(), c, &n).await?;
    if !d.writable {
        return Err(WebError::from(DomainError::ReadOnly));
    }
    let v = &d.card;
    html(FormPage {
        nav: Nav::new(&st, &user, "contacts"),
        new: false,
        action: href(c, &n),
        back: href(c, &n),
        etag: d.etag.clone(),
        books: Vec::new(),
        given: v.given.clone(),
        family: v.family.clone(),
        org: v.org.clone(),
        phones: rows_from(&v.phones, 2),
        emails: rows_from(&v.emails, 1),
        addresses: address_rows(&v.addresses),
        birthday: birthday_field(v.birthday),
        note: v.note.clone(),
        title: if v.name.trim().is_empty() { v.org.clone() } else { v.name.clone() },
    })
}

/// The posted form as a change. Repeated rows come as parallel lists
/// (`phone_kind`, `phone`, ...). The birthday changes only when it differs
/// from what the form showed (`birthday_was`), so a birthday without a year
/// is never lost by saving.
fn change_from(
    body: &[u8],
    a: &'static i18n::A,
) -> Result<(CardChange, std::collections::HashMap<String, String>), WebError> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_bytes(body).map_err(|_| WebError::bad(a.err_bad_form))?;
    let all = |k: &str| pairs.iter().filter(|(n, _)| n == k).map(|(_, v)| v.clone()).collect::<Vec<_>>();
    let one = |k: &str| pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default();
    let typed = |kinds: Vec<String>, values: Vec<String>| -> Vec<Typed> {
        values
            .into_iter()
            .enumerate()
            .map(|(i, v)| Typed::new(Kind::parse(kinds.get(i).map(String::as_str).unwrap_or("other")), v))
            .collect()
    };
    let (ak, street, pc, city, region, country) = (
        all("addr_kind"),
        all("addr_street"),
        all("addr_postcode"),
        all("addr_city"),
        all("addr_region"),
        all("addr_country"),
    );
    let get = |v: &Vec<String>, i: usize| v.get(i).cloned().unwrap_or_default();
    let addresses = (0..street.len())
        .map(|i| Address {
            kind: Some(Kind::parse(&get(&ak, i))).filter(|k| *k != Kind::Other),
            street: get(&street, i),
            postcode: get(&pc, i),
            city: get(&city, i),
            region: get(&region, i),
            country: get(&country, i),
            label: None,
        })
        .collect();
    let birthday = (one("birthday") != one("birthday_was"))
        .then(|| {
            let b = one("birthday");
            if b.trim().is_empty() {
                Ok(None)
            } else {
                NaiveDate::parse_from_str(b.trim(), "%Y-%m-%d")
                    .map(|d| Some(Birthday::Full(d)))
                    .map_err(|_| WebError::bad(a.err_bad_form))
            }
        })
        .transpose()?;
    let change = CardChange {
        given: Some(one("given")),
        family: Some(one("family")),
        org: Some(one("org")),
        phones: Some(typed(all("phone_kind"), all("phone"))),
        emails: Some(typed(all("email_kind"), all("email"))),
        addresses: Some(addresses),
        birthday,
        note: Some(one("note")),
        ..Default::default()
    };
    Ok((change, pairs.into_iter().collect()))
}

fn now() -> chrono::NaiveDateTime {
    chrono::Utc::now().naive_utc()
}

fn name_error(t: &'static i18n::T, e: DomainError) -> WebError {
    match e {
        DomainError::Invalid(m) if m.contains("name") => WebError::bad(t.need_name),
        other => other.into(),
    }
}

pub async fn create(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let t = i18n::t(user.lang);
    let (change, fields) = change_from(&body, i18n::a(user.lang))?;
    let book: Uuid = fields.get("book").and_then(|b| b.parse().ok()).ok_or_else(|| WebError::bad(t.no_book))?;
    let saved = contacts::create(&st.pool, user.id(), book, &change, now()).await.map_err(|e| name_error(t, e))?;
    Ok(Redirect::to(&format!("{}?added={}", href(book, &saved.name), saved.change_id)).into_response())
}

pub async fn update(
    State(st): State<AppState>,
    user: WebUser,
    Path((c, n)): Path<(Uuid, String)>,
    RawForm(body): RawForm,
) -> Page {
    let t = i18n::t(user.lang);
    let (change, fields) = change_from(&body, i18n::a(user.lang))?;
    let etag = fields.get("etag").cloned().unwrap_or_default();
    match contacts::update(&st.pool, user.id(), c, &n, &etag, &change, now()).await {
        Ok(saved) => Ok(Redirect::to(&format!("{}?saved={}", href(c, &n), saved.change_id)).into_response()),
        // Changed elsewhere: show the latest, never overwrite it.
        Err(DomainError::PreconditionFailed) => Ok(Redirect::to(&format!("{}?stale=1", href(c, &n))).into_response()),
        Err(e) => Err(name_error(t, e)),
    }
}

#[derive(Deserialize)]
pub struct DeleteForm {
    etag: String,
}

pub async fn delete(
    State(st): State<AppState>,
    user: WebUser,
    Path((c, n)): Path<(Uuid, String)>,
    axum::Form(f): axum::Form<DeleteForm>,
) -> Page {
    match contacts::remove(&st.pool, user.id(), c, &n, &f.etag).await {
        Ok(saved) => Ok(Redirect::to(&format!("/contacts?deleted={}", saved.change_id)).into_response()),
        Err(DomainError::PreconditionFailed) => Ok(Redirect::to(&format!("{}?stale=1", href(c, &n))).into_response()),
        Err(e) => Err(e.into()),
    }
}

/// `POST /contacts/undo/{change}`: back to the list (a restored contact is
/// in it again).
pub async fn undo(State(st): State<AppState>, user: WebUser, Path(change): Path<Uuid>) -> Page {
    let t = i18n::t(user.lang);
    match events::undo(&st.pool, user.id(), change).await {
        Ok(()) => Ok(Redirect::to("/contacts?undone=1").into_response()),
        Err(DomainError::PreconditionFailed) => Err(WebError::bad(t.msg_cannot_undo)),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_letters_and_links() {
        assert_eq!(initials("Ada Lovelace"), "AL");
        assert_eq!(initials("Ada Augusta King-Noel"), "AN");
        assert_eq!(initials("Pizzeria"), "PI");
        assert_eq!(initials("!!"), "?");
        assert_eq!(letter("émile"), "É");
        assert_eq!(letter("3M"), "#");
        assert_eq!(href(Uuid::nil(), "a b/c?.vcf"), "/contacts/00000000-0000-0000-0000-000000000000/a%20b%2Fc%3F.vcf");
        assert_eq!(color_for("Ada"), color_for("Ada"));
    }
}
