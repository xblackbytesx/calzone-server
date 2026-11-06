//! Calendars and address books: list, create, rename, recolour, share,
//! delete (with the name typed to confirm).

use askama::Template;
use axum::Form;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect};
use calzone_dav::path;
use serde::Deserialize;
use uuid::Uuid;

use super::session::WebUser;
use super::{Nav, Page, WebError, base_url, color_input, html, i18n};
use crate::AppState;
use crate::domain::accounts::{self, Account};
use crate::domain::collections::{self, Grant, slugify};
use crate::domain::{Access, Collection, CollectionKind, NewCollection};

/// One collection in a list.
pub struct Row {
    pub id: Uuid,
    pub name: String,
    pub color: String,
    /// "Shared by anna, can edit"; empty for your own.
    pub shared: String,
}

fn level_words(t: &'static i18n::T, a: Access) -> &'static str {
    match a {
        Access::Read => t.can_view,
        Access::Write => t.can_edit,
        Access::Admin | Access::Owner => t.can_share,
    }
}

fn display(c: &Collection) -> String {
    if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() }
}

fn row(c: &Collection, t: &'static i18n::T, a: &'static i18n::A) -> Row {
    let shared = if c.access == Access::Owner {
        String::new()
    } else {
        format!("{} {} · {}", a.shared_by, c.owner_name, level_words(t, c.access))
    };
    Row { id: c.id, name: display(c), color: color_input(c.color.as_deref()), shared }
}

#[derive(Template)]
#[template(path = "collections.html")]
struct ListPage {
    nav: Nav,
    calendars: Vec<Row>,
    books: Vec<Row>,
}

pub async fn list(State(st): State<AppState>, user: WebUser) -> Page {
    let cals = collections::visible(&st.pool, user.id(), CollectionKind::Calendar).await?;
    let books = collections::visible(&st.pool, user.id(), CollectionKind::AddressBook).await?;
    html(ListPage {
        nav: Nav::new(&st, &user, "collections"),
        calendars: cals.iter().map(|c| row(c, i18n::t(user.lang), i18n::a(user.lang))).collect(),
        books: books.iter().map(|c| row(c, i18n::t(user.lang), i18n::a(user.lang))).collect(),
    })
}

#[derive(Deserialize)]
pub struct CreateForm {
    kind: String,
    name: String,
    #[serde(default)]
    color: String,
}

fn valid_color(c: &str) -> Option<String> {
    let hex = c.strip_prefix('#')?;
    (hex.len() == 6 && hex.bytes().all(|b| b.is_ascii_hexdigit())).then(|| c.to_ascii_lowercase())
}

pub async fn create(State(st): State<AppState>, user: WebUser, Form(f): Form<CreateForm>) -> Page {
    let name = f.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(WebError::bad(i18n::a(user.lang).err_name_len));
    }
    let (kind, fallback) = match f.kind.as_str() {
        "calendar" => (CollectionKind::Calendar, "calendar"),
        "addressbook" => (CollectionKind::AddressBook, "contacts"),
        _ => return Err(WebError::bad(i18n::a(user.lang).err_kind)),
    };
    let color = if kind == CollectionKind::Calendar { valid_color(&f.color) } else { None };
    let new =
        NewCollection { kind, slug: "", display_name: name, description: "", color: color.as_deref(), components: &[] };
    // The slug is the DAV address; a taken one gets a number.
    let c = collections::create_numbered(&st.pool, user.id(), &slugify(name, fallback), new).await?;
    Ok(Redirect::to(&format!("/collections/{}", c.id)).into_response())
}

/// Someone to share with, for the picker.
pub struct Candidate {
    pub id: Uuid,
    pub label: String,
}

pub struct GrantRow {
    pub id: Uuid,
    pub label: String,
    pub level: &'static str,
}

#[derive(Template)]
#[template(path = "collection.html")]
struct ShowPage {
    nav: Nav,
    c: Collection,
    name: String,
    calendar: bool,
    color: String,
    level: &'static str,
    can_admin: bool,
    owner: bool,
    dav_url: Option<String>,
    grants: Vec<GrantRow>,
    candidates: Vec<Candidate>,
}

fn label(a: &Account) -> String {
    if a.is_group() { format!("{} (group)", a.label()) } else { format!("{} ({})", a.label(), a.name) }
}

pub async fn show(State(st): State<AppState>, user: WebUser, headers: HeaderMap, Path(id): Path<Uuid>) -> Page {
    let c = collections::get(&st.pool, user.id(), id).await?;
    let can_admin = c.access.can_admin();
    let (grants, candidates) = if can_admin {
        let grants: Vec<Grant> = collections::grants(&st.pool, user.id(), id).await?;
        let everyone = accounts::list(&st.pool).await?;
        let rows = grants
            .iter()
            .map(|g| GrantRow {
                id: g.principal_id,
                label: everyone.iter().find(|a| a.id == g.principal_id).map(label).unwrap_or_else(|| g.name.clone()),
                level: level_words(i18n::t(user.lang), g.level),
            })
            .collect();
        let candidates = everyone
            .iter()
            .filter(|a| a.id != c.owner_id && !a.disabled && !grants.iter().any(|g| g.principal_id == a.id))
            .map(|a| Candidate { id: a.id, label: label(a) })
            .collect();
        (rows, candidates)
    } else {
        (Vec::new(), Vec::new())
    };
    let calendar = c.kind == CollectionKind::Calendar;
    let dav_url = st.dav.then(|| {
        let kind = if calendar { path::Kind::Calendar } else { path::Kind::AddressBook };
        format!("{}{}", base_url(&st, &headers), path::collection_href(kind, &c.owner_name, &c.slug))
    });
    html(ShowPage {
        nav: Nav::new(&st, &user, "collections"),
        name: display(&c),
        calendar,
        color: color_input(c.color.as_deref()),
        level: level_words(i18n::t(user.lang), c.access),
        can_admin,
        owner: c.access == Access::Owner,
        dav_url,
        grants,
        candidates,
        c,
    })
}

#[derive(Deserialize)]
pub struct UpdateForm {
    name: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    color: String,
}

pub async fn update(
    State(st): State<AppState>,
    user: WebUser,
    Path(id): Path<Uuid>,
    Form(f): Form<UpdateForm>,
) -> Page {
    let name = f.name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(WebError::bad(i18n::a(user.lang).err_name_len));
    }
    if f.description.chars().count() > 1000 {
        return Err(WebError::bad(i18n::a(user.lang).err_desc_len));
    }
    let color = valid_color(&f.color);
    let c = collections::get(&st.pool, user.id(), id).await?;
    let color_change = if c.kind == CollectionKind::Calendar { color.as_deref().map(Some) } else { None };
    collections::update_properties(&st.pool, user.id(), id, Some(name), Some(f.description.trim()), color_change)
        .await?;
    Ok(Redirect::to(&format!("/collections/{id}")).into_response())
}

#[derive(Deserialize)]
pub struct ShareForm {
    principal: Uuid,
    level: String,
}

pub async fn share(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>, Form(f): Form<ShareForm>) -> Page {
    let level = Access::from_level(&f.level).ok_or_else(|| WebError::bad(i18n::a(user.lang).err_level))?;
    // Only people and groups that exist; anything else is not found.
    accounts::get(&st.pool, f.principal).await?;
    collections::set_grant(&st.pool, user.id(), id, f.principal, Some(level)).await?;
    Ok(Redirect::to(&format!("/collections/{id}")).into_response())
}

pub async fn unshare(State(st): State<AppState>, user: WebUser, Path((id, principal)): Path<(Uuid, Uuid)>) -> Page {
    collections::set_grant(&st.pool, user.id(), id, principal, None).await?;
    Ok(Redirect::to(&format!("/collections/{id}")).into_response())
}

#[derive(Deserialize)]
pub struct DeleteForm {
    confirm: String,
}

/// Deleting needs the collection's name typed exactly (case and outer
/// spaces aside): there is no undo yet, and a deleted calendar disappears
/// from every phone.
pub async fn delete(
    State(st): State<AppState>,
    user: WebUser,
    Path(id): Path<Uuid>,
    Form(f): Form<DeleteForm>,
) -> Page {
    let c = collections::get(&st.pool, user.id(), id).await?;
    if !f.confirm.trim().eq_ignore_ascii_case(display(&c).trim()) {
        return Err(WebError::bad(i18n::a(user.lang).err_type_exactly));
    }
    collections::delete(&st.pool, user.id(), id).await?;
    Ok(Redirect::to("/collections").into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Family 2026!", "calendar"), "family-2026");
        assert_eq!(slugify("  Wérk -- stuff ", "calendar"), "w-rk-stuff");
        assert_eq!(slugify("日本", "contacts"), "contacts");
        assert_eq!(slugify(&"a".repeat(80), "x").len(), 40);
        assert!(crate::domain::valid_slug(&slugify("Ünïcode Ñame", "x")));
    }

    #[test]
    fn colours() {
        assert_eq!(valid_color("#AABBCC").as_deref(), Some("#aabbcc"));
        assert_eq!(valid_color("#abc"), None);
        assert_eq!(valid_color("javascript:x"), None);
    }
}
