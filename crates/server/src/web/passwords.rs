//! The Devices page (decision 10): phones with the CalZone app, signed in
//! with a one-time QR code (decision 47), and app passwords for DAV clients,
//! shown only while the DAV door is on.

use std::collections::HashMap;

use askama::Template;
use axum::extract::{Path, RawForm, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Redirect};
use uuid::Uuid;

use super::session::WebUser;
use super::{Nav, Page, WebError, base_url, html, i18n};
use crate::AppState;
use crate::domain::app_passwords::{self, Scope};
use crate::domain::devices;
use crate::domain::{Access, Collection, CollectionKind, collections};

pub struct DeviceRow {
    pub id: Uuid,
    pub label: String,
    pub scope: String,
    pub created: String,
    pub used: String,
}

pub struct PhoneRow {
    pub id: Uuid,
    pub label: String,
    pub push: bool,
    pub created: String,
    pub used: String,
}

pub struct Choice {
    pub id: Uuid,
    pub label: String,
    pub writable: bool,
}

#[derive(Template)]
#[template(path = "devices.html")]
struct DevicesPage {
    nav: Nav,
    phones: Vec<PhoneRow>,
    devices: Vec<DeviceRow>,
    choices: Vec<Choice>,
}

async fn everything(st: &AppState, user: &WebUser) -> Result<Vec<Collection>, WebError> {
    let mut all = collections::visible(&st.pool, user.id(), CollectionKind::Calendar).await?;
    all.extend(collections::visible(&st.pool, user.id(), CollectionKind::AddressBook).await?);
    Ok(all)
}

fn name(c: &Collection, a: &'static i18n::A) -> String {
    let n = if c.display_name.is_empty() { &c.slug } else { &c.display_name };
    let kind = if c.kind == CollectionKind::Calendar { a.word_calendar } else { a.word_book };
    if c.access == Access::Owner { format!("{n} ({kind})") } else { format!("{n} ({kind}, {})", c.owner_name) }
}

pub async fn list(State(st): State<AppState>, user: WebUser) -> Page {
    let a = i18n::a(user.lang);
    let all = everything(&st, &user).await?;
    let mut devices = Vec::new();
    for p in app_passwords::list(&st.pool, user.id()).await? {
        let scope = match &p.scope {
            None => a.everything.to_string(),
            Some(s) => {
                let mut parts: Vec<String> = all
                    .iter()
                    .filter_map(|c| {
                        s.get(&c.id).map(|acc| {
                            format!("{} {}", name(c, a), if acc.can_write() { a.tag_edit } else { a.tag_view })
                        })
                    })
                    .collect();
                parts.sort();
                if parts.is_empty() { a.nothing_any_more.to_string() } else { parts.join(", ") }
            }
        };
        devices.push(DeviceRow {
            id: p.id,
            label: p.label,
            scope,
            created: p.created_at.format("%Y-%m-%d").to_string(),
            used: p.last_used_at.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| a.never.into()),
        });
    }
    let phones = devices::list(&st.pool, user.id())
        .await?
        .into_iter()
        .map(|d| PhoneRow {
            id: d.id,
            label: d.label,
            push: d.push,
            created: d.created_at.format("%Y-%m-%d").to_string(),
            used: d.last_used_at.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string()).unwrap_or_else(|| a.never.into()),
        })
        .collect();
    let choices = all.iter().map(|c| Choice { id: c.id, label: name(c, a), writable: c.access.can_write() }).collect();
    html(DevicesPage { nav: Nav::new(&st, &user, "devices"), phones, devices, choices })
}

#[derive(Template)]
#[template(path = "device_created.html")]
struct CreatedPage {
    nav: Nav,
    label: String,
    user_name: String,
    password: String,
    server: String,
}

/// The form: `label`, `limit` (`all` or `some`), and per collection
/// `c-<id>` = `none`, `read` or `write`. Read raw, since the field names
/// are dynamic.
pub async fn create(State(st): State<AppState>, user: WebUser, headers: HeaderMap, RawForm(body): RawForm) -> Page {
    let fields: HashMap<String, String> = serde_urlencoded::from_bytes::<Vec<(String, String)>>(&body)
        .map_err(|_| WebError::bad(i18n::a(user.lang).err_bad_form))?
        .into_iter()
        .collect();
    let label = fields.get("label").map(|s| s.trim().to_string()).unwrap_or_default();
    let scope: Option<Scope> = if fields.get("limit").map(String::as_str) == Some("some") {
        let mut s = Scope::new();
        for (k, v) in &fields {
            let Some(id) = k.strip_prefix("c-").and_then(|i| i.parse::<Uuid>().ok()) else { continue };
            match v.as_str() {
                "read" => s.insert(id, Access::Read),
                "write" => s.insert(id, Access::Write),
                _ => None,
            };
        }
        if s.is_empty() {
            return Err(WebError::bad(i18n::a(user.lang).err_pick_one));
        }
        Some(s)
    } else {
        None
    };
    let (_, password) = app_passwords::create(&st.pool, user.id(), &label, scope.as_ref()).await?;
    html(CreatedPage {
        nav: Nav::new(&st, &user, "devices"),
        label,
        user_name: user.account.name.clone(),
        password,
        server: base_url(&st, &headers),
    })
}

pub async fn revoke(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>) -> Page {
    app_passwords::revoke(&st.pool, user.id(), id).await?;
    Ok(Redirect::to("/devices").into_response())
}

#[derive(Template)]
#[template(path = "device_link.html")]
struct LinkPage {
    nav: Nav,
    qr: String,
    url: String,
}

/// A one-time sign-in code for the CalZone app, as a QR code and as an
/// address to paste (10 minutes). It comes from a signed-in session, so the
/// phone is not asked for two-factor again.
pub async fn create_link(State(st): State<AppState>, user: WebUser, headers: HeaderMap) -> Page {
    let (code, _) = devices::create_link(&st.pool, user.id()).await?;
    let url = format!("{}/link/{code}", base_url(&st, &headers));
    html(LinkPage { nav: Nav::new(&st, &user, "devices"), qr: crate::domain::two_factor::qr_svg(&url), url })
}

/// Sign a phone out: its token stops working at once.
pub async fn remove_phone(State(st): State<AppState>, user: WebUser, Path(id): Path<Uuid>) -> Page {
    devices::remove(&st.pool, user.id(), id).await?;
    Ok(Redirect::to("/devices").into_response())
}

#[derive(Template)]
#[template(path = "link_landing.html")]
struct Landing {
    lang: &'static str,
    a: &'static i18n::A,
}

/// The QR code's address opened in a browser (a camera app, say): it only
/// works in the CalZone app. The code is not looked at, so this page says
/// nothing about whether it is still good.
pub async fn link_landing(headers: HeaderMap, Path(_code): Path<String>) -> Page {
    let lang = i18n::lang(None, &headers);
    html(Landing { lang: i18n::code(lang), a: i18n::a(lang) })
}
