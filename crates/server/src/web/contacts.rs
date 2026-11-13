//! Contacts pages (phase 2 step 4): a searchable list, one contact, and
//! the edit form. Plain forms with post-redirect-get (they work without
//! JavaScript; HTMX boosts them), the search live through `hx-get`.
//! Deleting and saving show a notice with Undo instead of a confirm
//! dialog (decision 12).

use askama::Template;
use axum::extract::{Path, Query, RawForm, RawQuery, State};
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
use calzone_core::dedupe::Reason;

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

pub(crate) fn href(collection: Uuid, name: &str) -> String {
    format!("/contacts/{collection}/{}", utf8_percent_encode(name, SEGMENT))
}

const PALETTE: [&str; 8] = ["#3a7bd5", "#c2185b", "#00897b", "#f4511e", "#7e57c2", "#43a047", "#8d6e63", "#1e88e5"];

/// A stable color for someone without a photo.
fn color_for(name: &str) -> &'static str {
    let h = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ u32::from(b));
    PALETTE[(h % PALETTE.len() as u32) as usize]
}

pub use calzone_core::view::initials;

/// A list letter: the first letter, `#` for anything else.
fn letter(name: &str) -> String {
    match name.trim().chars().next() {
        Some(c) if c.is_alphabetic() => c.to_uppercase().collect::<String>().chars().take(1).collect(),
        _ => "#".into(),
    }
}

pub struct RowView {
    pub href: String,
    /// `collection/name`, for picking it.
    pub pick: String,
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
    pub writable: bool,
    pub count: usize,
    pub groups: Vec<Group>,
}

/// A notice from the redirect after a write, with an Undo when it applies.
pub struct Notice {
    pub text: String,
    pub undo: Option<Uuid>,
    pub warn: bool,
}

#[derive(Deserialize, Default)]
pub struct Flash {
    #[serde(default)]
    q: String,
    book: Option<String>,
    name: Option<String>,
    phone: Option<String>,
    email: Option<String>,
    address: Option<String>,
    birthday: Option<String>,
    photo: Option<String>,
    org: Option<String>,
    select: Option<String>,
    saved: Option<Uuid>,
    added: Option<Uuid>,
    deleted: Option<Uuid>,
    /// How many a bulk delete removed.
    n: Option<usize>,
    merged: Option<Uuid>,
    undone: Option<String>,
    stale: Option<String>,
}

impl Flash {
    fn filter(&self) -> contacts::Filter {
        let w = |x: &Option<String>| contacts::Want::parse(x.as_deref().unwrap_or(""));
        contacts::Filter {
            q: self.q.clone(),
            book: self.book.as_deref().and_then(|b| b.parse().ok()),
            name: w(&self.name),
            phone: w(&self.phone),
            email: w(&self.email),
            address: w(&self.address),
            birthday: w(&self.birthday),
            photo: w(&self.photo),
            org: w(&self.org),
        }
    }
}

fn notice(t: &'static i18n::T, c: &'static i18n::C, f: &Flash) -> Option<Notice> {
    if let (Some(id), Some(n)) = (f.deleted, f.n) {
        let text = if n == 1 { c.deleted_one.to_string() } else { i18n::n(c.deleted_many, n) };
        return Some(Notice { text, undo: Some(id), warn: false });
    }
    if let Some(id) = f.deleted {
        return Some(Notice { text: t.contact_deleted.into(), undo: Some(id), warn: false });
    }
    if let Some(id) = f.merged {
        return Some(Notice { text: c.merged.into(), undo: Some(id), warn: false });
    }
    if let Some(id) = f.saved {
        return Some(Notice { text: t.contact_saved.into(), undo: Some(id), warn: false });
    }
    if let Some(id) = f.added {
        return Some(Notice { text: t.contact_added.into(), undo: Some(id), warn: false });
    }
    if f.undone.is_some() {
        return Some(Notice { text: t.undone.into(), undo: None, warn: false });
    }
    if f.stale.is_some() {
        return Some(Notice { text: t.stale_contact.into(), undo: None, warn: true });
    }
    None
}

/// One has/lacks choice in the filter.
pub struct FilterField {
    pub key: &'static str,
    pub label: &'static str,
    pub value: &'static str,
}

#[derive(Template)]
#[template(path = "contacts.html")]
struct ListPage {
    nav: Nav,
    c: &'static i18n::C,
    q: String,
    book: String,
    book_choices: Vec<(Uuid, String)>,
    fields: Vec<FilterField>,
    /// Field filters (and a book) set: the filter box opens.
    filtering: usize,
    /// The filter as a query string, without `select`.
    query: String,
    /// The search alone (filters cleared).
    clear_href: String,
    select: bool,
    books: Vec<BookView>,
    total: usize,
    shown: String,
    duplicates: usize,
    can_add: bool,
    can_pick: bool,
    notice: Option<Notice>,
}

fn filter_query(f: &contacts::Filter) -> String {
    let mut parts: Vec<(&str, String)> = Vec::new();
    if !f.q.trim().is_empty() {
        parts.push(("q", f.q.clone()));
    }
    if let Some(b) = f.book {
        parts.push(("book", b.to_string()));
    }
    for (k, w) in [
        ("name", f.name),
        ("phone", f.phone),
        ("email", f.email),
        ("address", f.address),
        ("birthday", f.birthday),
        ("photo", f.photo),
        ("org", f.org),
    ] {
        if w != contacts::Want::Any {
            parts.push((k, w.as_str().into()));
        }
    }
    serde_urlencoded::to_string(&parts).unwrap_or_default()
}

pub async fn list(State(st): State<AppState>, user: WebUser, Query(f): Query<Flash>) -> Page {
    let t = i18n::t(user.lang);
    let c = i18n::c(user.lang);
    let filter = f.filter();
    let books = contacts::list(&st.pool, user.id(), &filter).await?;
    let can_add = books.iter().any(|b| b.writable);
    let duplicates = books.iter().map(|b| b.duplicates).sum();
    let select = f.select.is_some() && can_add;
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
                    pick: format!("{}/{}", r.collection, r.name),
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
            BookView { name: b.name, owner: b.owner, own: b.own, writable: b.writable, count, groups }
        })
        .collect::<Vec<_>>();
    let can_pick = views.iter().any(|b| b.writable && b.count > 0);
    let fields = vec![
        FilterField { key: "name", label: c.f_name, value: filter.name.as_str() },
        FilterField { key: "phone", label: c.f_phone, value: filter.phone.as_str() },
        FilterField { key: "email", label: c.f_email, value: filter.email.as_str() },
        FilterField { key: "address", label: c.f_address, value: filter.address.as_str() },
        FilterField { key: "birthday", label: c.f_birthday, value: filter.birthday.as_str() },
        FilterField { key: "photo", label: c.f_photo, value: filter.photo.as_str() },
        FilterField { key: "org", label: c.f_org, value: filter.org.as_str() },
    ];
    html(ListPage {
        nav: Nav::new(&st, &user, "contacts"),
        c,
        q: f.q.clone(),
        book: filter.book.map(|b| b.to_string()).unwrap_or_default(),
        book_choices: contacts::books(&st.pool, user.id()).await?,
        fields,
        filtering: filter.fields_set() + usize::from(filter.book.is_some()),
        query: filter_query(&filter),
        clear_href: {
            let mut parts: Vec<(&str, String)> = Vec::new();
            if !f.q.trim().is_empty() {
                parts.push(("q", f.q.clone()));
            }
            if select {
                parts.push(("select", "1".into()));
            }
            format!("/contacts?{}", serde_urlencoded::to_string(&parts).unwrap_or_default())
        },
        select,
        books: views,
        total,
        shown: i18n::n(c.shown, total),
        duplicates,
        can_add,
        can_pick,
        notice: notice(t, c, &f),
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
        notice: notice(t, i18n::c(user.lang), &f),
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

// ---- Clean-up (decision 56): bulk delete, duplicates, merge ----

fn form_pairs(body: &[u8], a: &'static i18n::A) -> Result<Vec<(String, String)>, WebError> {
    serde_urlencoded::from_bytes(body).map_err(|_| WebError::bad(a.err_bad_form))
}

fn all_of(pairs: &[(String, String)], k: &str) -> Vec<String> {
    pairs.iter().filter(|(n, _)| n == k).map(|(_, v)| v.clone()).collect()
}

fn one_of(pairs: &[(String, String)], k: &str) -> String {
    pairs.iter().find(|(n, _)| n == k).map(|(_, v)| v.clone()).unwrap_or_default()
}

/// `collection/name` back into its parts.
fn parse_pick(s: &str) -> Option<(Uuid, String)> {
    let (c, n) = s.split_once('/')?;
    Some((c.parse().ok()?, n.to_string())).filter(|(_, n)| !n.is_empty())
}

fn merge_href(book: Uuid, names: &[String]) -> String {
    let mut parts: Vec<(&str, String)> = vec![("book", book.to_string())];
    parts.extend(names.iter().map(|n| ("n", n.clone())));
    format!("/contacts/merge?{}", serde_urlencoded::to_string(&parts).unwrap_or_default())
}

pub struct PickedView {
    pub collection: Uuid,
    pub name: String,
    pub etag: String,
    pub display: String,
    pub book: String,
}

#[derive(Template)]
#[template(path = "contacts_delete.html")]
struct DeletePage {
    nav: Nav,
    c: &'static i18n::C,
    title: String,
    confirm: String,
    button: String,
    picked: Vec<PickedView>,
    several_books: bool,
}

/// `POST /contacts/selected`: from select mode, to the delete check or the
/// merge page. Nothing changes here.
pub async fn selected(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let c = i18n::c(user.lang);
    let pairs = form_pairs(&body, i18n::a(user.lang))?;
    let refs: Vec<(Uuid, String)> = all_of(&pairs, "pick").iter().filter_map(|p| parse_pick(p)).collect();
    if refs.is_empty() {
        return Err(WebError::bad(c.nothing_selected));
    }
    if one_of(&pairs, "action") == "merge" {
        let book = refs[0].0;
        let mut names: Vec<String> = Vec::new();
        for (b, n) in &refs {
            if *b != book {
                return Err(WebError::bad(c.merge_needs));
            }
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        if !(2..=calzone_core::dedupe::MAX_MERGE).contains(&names.len()) {
            return Err(WebError::bad(c.merge_needs));
        }
        return Ok(Redirect::to(&merge_href(book, &names)).into_response());
    }
    let picked = contacts::picked(&st.pool, user.id(), &refs).await?;
    if picked.is_empty() {
        return Err(WebError::bad(c.nothing_selected));
    }
    let n = picked.len();
    let several_books = picked.iter().any(|p| p.book != picked[0].book);
    html(DeletePage {
        nav: Nav::new(&st, &user, "contacts"),
        c,
        title: if n == 1 { c.delete_title_one.into() } else { i18n::n(c.delete_title, n) },
        confirm: if n == 1 { c.delete_confirm_one.into() } else { i18n::n(c.delete_confirm, n) },
        button: if n == 1 { c.delete_button_one.into() } else { i18n::n(c.delete_button, n) },
        picked: picked
            .into_iter()
            .map(|p| PickedView {
                collection: p.collection,
                name: p.name,
                etag: p.etag,
                display: p.display,
                book: p.book,
            })
            .collect(),
        several_books,
    })
}

/// `POST /contacts/delete-many`: the checked list, under the ETags the
/// check page showed, only with the box ticked. One change, one Undo.
pub async fn delete_many(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let c = i18n::c(user.lang);
    let pairs = form_pairs(&body, i18n::a(user.lang))?;
    if one_of(&pairs, "confirm") != "yes" {
        return Err(WebError::bad(c.need_confirm));
    }
    let (cs, ns, es) = (all_of(&pairs, "c"), all_of(&pairs, "n"), all_of(&pairs, "e"));
    if cs.is_empty() || cs.len() != ns.len() || cs.len() != es.len() {
        return Err(WebError::bad(c.nothing_selected));
    }
    let mut targets = Vec::with_capacity(cs.len());
    for ((col, name), etag) in cs.iter().zip(ns).zip(es) {
        let collection: Uuid = col.parse().map_err(|_| WebError::bad(c.nothing_selected))?;
        targets.push(contacts::Target { collection, name, etag });
    }
    match contacts::remove_many(&st.pool, user.id(), &targets).await {
        Ok(id) => Ok(Redirect::to(&format!("/contacts?deleted={id}&n={}", targets.len())).into_response()),
        Err(DomainError::PreconditionFailed | DomainError::NotFound) => Err(WebError::bad(c.changed_since)),
        Err(e) => Err(e.into()),
    }
}

pub struct DupGroupView {
    pub rows: Vec<RowView>,
    pub reasons: Vec<&'static str>,
    pub name_only: bool,
    pub merge: Option<String>,
    pub names: Vec<String>,
}

pub struct DupBookView {
    pub id: Uuid,
    pub name: String,
    pub owner: String,
    pub own: bool,
    pub groups: Vec<DupGroupView>,
}

/// The Undo after "Not duplicates".
pub struct ApartNotice {
    pub book: Uuid,
    pub names: Vec<String>,
}

#[derive(Template)]
#[template(path = "contacts_duplicates.html")]
struct DupPage {
    nav: Nav,
    c: &'static i18n::C,
    books: Vec<DupBookView>,
    several: bool,
    total: usize,
    apart: Option<ApartNotice>,
}

/// `GET /contacts/duplicates`.
pub async fn duplicates(State(st): State<AppState>, user: WebUser, RawQuery(q): RawQuery) -> Page {
    let c = i18n::c(user.lang);
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(q.as_deref().unwrap_or("")).unwrap_or_default();
    let apart = one_of(&pairs, "apart")
        .parse::<Uuid>()
        .ok()
        .map(|book| ApartNotice { book, names: all_of(&pairs, "n") })
        .filter(|a| a.names.len() > 1);
    let books = contacts::duplicates(&st.pool, user.id()).await?;
    let several = books.len() > 1;
    let mut total = 0;
    let books = books
        .into_iter()
        .map(|b| {
            total += b.groups.len();
            let groups = b
                .groups
                .into_iter()
                .map(|g| {
                    let names: Vec<String> = g.cards.iter().map(|r| r.name.clone()).collect();
                    let rows = g
                        .cards
                        .into_iter()
                        .map(|r| {
                            let sub = [r.phone.clone().unwrap_or_default(), r.email.clone().unwrap_or_default()]
                                .into_iter()
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join(" · ");
                            RowView {
                                href: href(r.collection, &r.name),
                                pick: format!("{}/{}", r.collection, r.name),
                                initials: initials(&r.display),
                                color: color_for(&r.display),
                                photo: r.has_photo.then(|| {
                                    format!(
                                        "/contacts/photo/{}/{}",
                                        r.collection,
                                        utf8_percent_encode(&r.name, SEGMENT)
                                    )
                                }),
                                display: r.display,
                                sub,
                            }
                        })
                        .collect();
                    let reasons = g
                        .reasons
                        .iter()
                        .map(|r| match r {
                            Reason::Phone => c.reason_phone,
                            Reason::Email => c.reason_email,
                            Reason::Name => c.reason_name,
                        })
                        .collect();
                    DupGroupView {
                        rows,
                        reasons,
                        name_only: g.reasons == [Reason::Name],
                        merge: (names.len() <= calzone_core::dedupe::MAX_MERGE).then(|| merge_href(b.id, &names)),
                        names,
                    }
                })
                .collect();
            DupBookView { id: b.id, name: b.name, owner: b.owner, own: b.own, groups }
        })
        .collect();
    html(DupPage { nav: Nav::new(&st, &user, "contacts"), c, books, several, total, apart })
}

async fn set_apart(st: &AppState, user: &WebUser, body: &[u8], apart: bool) -> Result<(Uuid, Vec<String>), WebError> {
    let pairs = form_pairs(body, i18n::a(user.lang))?;
    let book: Uuid = one_of(&pairs, "book").parse().map_err(|_| WebError::bad(i18n::a(user.lang).err_bad_form))?;
    let names = all_of(&pairs, "n");
    contacts::set_apart(&st.pool, user.id(), book, &names, apart).await?;
    Ok((book, names))
}

/// `POST /contacts/duplicates/apart`: "Not duplicates", with an Undo.
pub async fn mark_apart(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let (book, names) = set_apart(&st, &user, &body, true).await?;
    let mut parts: Vec<(&str, String)> = vec![("apart", book.to_string())];
    parts.extend(names.into_iter().map(|n| ("n", n)));
    let q = serde_urlencoded::to_string(&parts).unwrap_or_default();
    Ok(Redirect::to(&format!("/contacts/duplicates?{q}")).into_response())
}

/// `POST /contacts/duplicates/together`: that Undo.
pub async fn unmark_apart(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    set_apart(&st, &user, &body, false).await?;
    Ok(Redirect::to("/contacts/duplicates").into_response())
}

pub struct MergeCardView {
    pub name: String,
    pub etag: String,
    pub display: String,
    pub sub: String,
    pub initials: String,
    pub color: &'static str,
    pub photo: Option<String>,
    pub kept: bool,
    pub keep_href: String,
}

/// A radio or checkbox on the merge page: its value, what it says, and
/// where it comes from.
pub struct Choice {
    pub value: String,
    pub label: String,
    pub meta: String,
    pub checked: bool,
}

pub struct PhotoChoice {
    pub value: String,
    pub src: Option<String>,
    pub checked: bool,
}

#[derive(Template)]
#[template(path = "contact_merge.html")]
struct MergePage {
    nav: Nav,
    c: &'static i18n::C,
    title: String,
    button: String,
    book: Uuid,
    book_name: String,
    keep: usize,
    cards: Vec<MergeCardView>,
    names: Vec<Choice>,
    orgs: Vec<Choice>,
    titles: Vec<Choice>,
    birthdays: Vec<Choice>,
    photos: Vec<PhotoChoice>,
    phones: Vec<Choice>,
    emails: Vec<Choice>,
    addresses: Vec<Choice>,
    groups: Vec<Choice>,
    note: String,
    stale: bool,
}

/// `GET /contacts/merge?book=..&n=..&n=..[&keep=i]`.
pub async fn merge_form(State(st): State<AppState>, user: WebUser, RawQuery(q): RawQuery) -> Page {
    let t = i18n::t(user.lang);
    let c = i18n::c(user.lang);
    let a = i18n::a(user.lang);
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(q.as_deref().unwrap_or("")).unwrap_or_default();
    let book: Uuid = one_of(&pairs, "book").parse().map_err(|_| WebError::bad(a.err_bad_form))?;
    let set = match contacts::merge_set(&st.pool, user.id(), book, &all_of(&pairs, "n")).await {
        Ok(s) => s,
        Err(DomainError::Invalid(_)) => return Err(WebError::bad(c.merge_needs)),
        Err(e) => return Err(e.into()),
    };
    let keep = one_of(&pairs, "keep")
        .parse::<usize>()
        .ok()
        .filter(|k| *k < set.cards.len())
        .unwrap_or_else(|| calzone_core::dedupe::keeper(&set.cards));
    let p = calzone_core::dedupe::proposal(&set.cards, keep);
    let picks = calzone_core::dedupe::Picks::all(&p);
    let shown = |v: &card::CardView| {
        if v.name.trim().is_empty() { v.org.clone() } else { v.name.clone() }
    };
    let from = |i: usize| c.from_card.replace("{name}", &shown(&set.cards[i]));
    let photo_src = |i: usize| {
        matches!(set.cards[i].photo, Some(card::Photo::Inline { .. }))
            .then(|| format!("/contacts/photo/{book}/{}", utf8_percent_encode(&set.names[i], SEGMENT)))
    };
    let cards = set
        .cards
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let d = shown(v);
            let mut sub: Vec<String> = Vec::new();
            sub.extend(v.phones.first().map(|p| p.value.clone()));
            sub.extend(v.emails.first().map(|e| e.value.clone()));
            let mut parts: Vec<(&str, String)> = vec![("book", book.to_string())];
            parts.extend(set.names.iter().map(|n| ("n", n.clone())));
            parts.push(("keep", i.to_string()));
            MergeCardView {
                name: set.names[i].clone(),
                etag: set.etags[i].clone(),
                initials: initials(&d),
                color: color_for(&d),
                photo: photo_src(i),
                display: d,
                sub: sub.join(" · "),
                kept: i == keep,
                keep_href: format!("/contacts/merge?{}", serde_urlencoded::to_string(&parts).unwrap_or_default()),
            }
        })
        .collect();
    let single = |list: Vec<(String, String)>, picked: Option<usize>| -> Vec<Choice> {
        list.into_iter()
            .enumerate()
            .map(|(i, (label, meta))| Choice { value: i.to_string(), label, meta, checked: picked == Some(i) })
            .collect()
    };
    let names = p
        .names
        .iter()
        .map(|&i| {
            let v = &set.cards[i];
            let parts = [v.prefix.as_str(), &v.given, &v.middle, &v.family, &v.suffix]
                .iter()
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            // The name parts, when they say more than the shown name.
            let meta = if parts == shown(v) { String::new() } else { parts };
            Choice { value: i.to_string(), label: shown(v), meta, checked: picks.name == i }
        })
        .collect();
    let orgs = single(p.orgs.iter().map(|(i, v)| (v.clone(), from(*i))).collect(), picks.org);
    let titles = single(p.titles.iter().map(|(i, v)| (v.clone(), from(*i))).collect(), picks.title);
    let birthdays =
        single(p.birthdays.iter().map(|(i, b)| (birthday_text(user.lang, *b), from(*i))).collect(), picks.birthday);
    let photos = p
        .photos
        .iter()
        .map(|&i| PhotoChoice { value: i.to_string(), src: photo_src(i), checked: picks.photo == Some(i) })
        .collect();
    let many = |list: Vec<(String, String)>| -> Vec<Choice> {
        list.into_iter()
            .enumerate()
            .map(|(i, (label, meta))| Choice { value: i.to_string(), label, meta, checked: true })
            .collect()
    };
    let typed_meta = |i: usize, x: &card::Typed| {
        let kind = x.label.clone().unwrap_or_else(|| kind_word(t, x.kind).to_string());
        format!("{kind} · {}", from(i))
    };
    let phones = many(p.phones.iter().map(|(i, x)| (x.value.clone(), typed_meta(*i, x))).collect());
    let emails = many(p.emails.iter().map(|(i, x)| (x.value.clone(), typed_meta(*i, x))).collect());
    let addresses = many(
        p.addresses
            .iter()
            .map(|(i, a)| {
                let kind = a.label.clone().unwrap_or_else(|| kind_word(t, a.kind.unwrap_or(Kind::Other)).to_string());
                (a.one_line(), format!("{kind} · {}", from(*i)))
            })
            .collect(),
    );
    let groups = many(p.categories.iter().map(|g| (g.clone(), String::new())).collect());
    let n = set.cards.len();
    html(MergePage {
        nav: Nav::new(&st, &user, "contacts"),
        c,
        title: i18n::n(c.merge_title, n),
        button: i18n::n(c.merge_button, n),
        book,
        book_name: set.book,
        keep,
        cards,
        names,
        orgs,
        titles,
        birthdays,
        photos,
        phones,
        emails,
        addresses,
        groups,
        note: picks.note,
        stale: one_of(&pairs, "stale") == "1",
    })
}

/// `POST /contacts/merge`: one change, then the kept contact with Undo.
pub async fn merge(State(st): State<AppState>, user: WebUser, RawForm(body): RawForm) -> Page {
    let a = i18n::a(user.lang);
    let c = i18n::c(user.lang);
    let pairs = form_pairs(&body, a)?;
    let book: Uuid = one_of(&pairs, "book").parse().map_err(|_| WebError::bad(a.err_bad_form))?;
    let names = all_of(&pairs, "n");
    let etags = all_of(&pairs, "e");
    let keep: usize = one_of(&pairs, "keep").parse().map_err(|_| WebError::bad(a.err_bad_form))?;
    let index = |k: &str| one_of(&pairs, k).parse::<usize>().ok();
    let indices = |k: &str| all_of(&pairs, k).iter().filter_map(|v| v.parse::<usize>().ok()).collect::<Vec<_>>();
    let picks = calzone_core::dedupe::Picks {
        name: index("name").unwrap_or(keep),
        org: index("org"),
        title: index("title"),
        birthday: index("birthday"),
        photo: index("photo"),
        phones: indices("phone"),
        emails: indices("email"),
        addresses: indices("addr"),
        categories: indices("group"),
        note: one_of(&pairs, "note"),
    };
    match contacts::merge(&st.pool, user.id(), book, &names, &etags, keep, &picks, now()).await {
        Ok(id) => {
            let kept = names.get(keep).cloned().unwrap_or_default();
            Ok(Redirect::to(&format!("{}?merged={id}", href(book, &kept))).into_response())
        }
        Err(DomainError::PreconditionFailed | DomainError::NotFound) => {
            let mut parts: Vec<(&str, String)> = vec![("book", book.to_string())];
            parts.extend(names.iter().map(|n| ("n", n.clone())));
            parts.push(("keep", keep.to_string()));
            parts.push(("stale", "1".into()));
            Ok(Redirect::to(&format!("/contacts/merge?{}", serde_urlencoded::to_string(&parts).unwrap_or_default()))
                .into_response())
        }
        Err(DomainError::Invalid(m)) if m.contains("name") => Err(WebError::bad(i18n::t(user.lang).need_name)),
        Err(DomainError::Invalid(_)) => Err(WebError::bad(c.merge_needs)),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_letters_and_links() {
        assert_eq!(letter("émile"), "É");
        assert_eq!(letter("3M"), "#");
        assert_eq!(href(Uuid::nil(), "a b/c?.vcf"), "/contacts/00000000-0000-0000-0000-000000000000/a%20b%2Fc%3F.vcf");
        assert_eq!(color_for("Ada"), color_for("Ada"));
    }
}
