//! ICS feeds in the web app (phase 2 step 6, decision 41): the public feed
//! behind a secret link, the JSON endpoints that make and revoke links,
//! and subscribing to an outside `.ics` (fetched under the SSRF guard in
//! `server::feeds`).

use askama::Template;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_DISPOSITION, CONTENT_TYPE, ETAG, IF_NONE_MATCH, LAST_MODIFIED};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use calzone_core::feed::Detail;
use calzone_core::quickadd::Lang;
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use super::api::{ApiError, ApiResult};
use super::session::{ApiUser, WebUser};
use super::{Page, html, i18n};
use crate::AppState;
use crate::domain::subscriptions::{self, INTERVALS};
use crate::domain::{DomainError, collections, feeds as links, prefs};
use crate::feeds::{self, Fetched, Outcome};

/// The busy title in the language of whoever made the link.
fn busy_title(language: Option<&str>) -> &'static str {
    let lang = if language == Some("nl") { Lang::Nl } else { Lang::En };
    i18n::t(lang).busy_title
}

/// `GET /feeds/{token}.ics`: no session; the token is the key. Unknown or
/// revoked is a plain 404.
pub async fn feed(State(st): State<AppState>, Path(file): Path<String>, headers: HeaderMap) -> Response {
    let token = file.strip_suffix(".ics").unwrap_or(&file);
    if token.is_empty() || token.len() > 100 {
        return (StatusCode::NOT_FOUND, "Not found.").into_response();
    }
    let p = match links::published(&st.pool, token, busy_title).await {
        Ok(p) => p,
        Err(DomainError::NotFound) => return (StatusCode::NOT_FOUND, "Not found.").into_response(),
        Err(e) => {
            tracing::error!(error = %e, "feeds: serving a feed failed");
            return (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server.").into_response();
        }
    };
    let mut h = HeaderMap::new();
    h.insert(CACHE_CONTROL, HeaderValue::from_static("private, no-cache"));
    h.insert("x-robots-tag", HeaderValue::from_static("noindex"));
    if let Ok(v) = HeaderValue::from_str(&p.etag) {
        h.insert(ETAG, v);
    }
    if let Some(lm) = p.last_modified
        && let Ok(v) = HeaderValue::from_str(&lm.format("%a, %d %b %Y %H:%M:%S GMT").to_string())
    {
        h.insert(LAST_MODIFIED, v);
    }
    let fresh = headers
        .get(IF_NONE_MATCH)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|e| e.trim().trim_start_matches("W/") == p.etag));
    if fresh {
        return (StatusCode::NOT_MODIFIED, h).into_response();
    }
    h.insert(CONTENT_TYPE, HeaderValue::from_static("text/calendar; charset=utf-8"));
    if let Ok(v) = HeaderValue::from_str(&format!("inline; filename=\"{}\"", p.file_name.replace('"', ""))) {
        h.insert(CONTENT_DISPOSITION, v);
    }
    (h, p.body).into_response()
}

/// The person's language for messages made here.
async fn lang_of(st: &AppState, user: Uuid, headers: &HeaderMap) -> Result<Lang, ApiError> {
    let s = prefs::settings(&st.pool, user).await?;
    Ok(i18n::lang(s.language.as_deref(), headers))
}

#[derive(Deserialize)]
pub struct LinkBody {
    #[serde(default)]
    label: String,
    detail: String,
}

/// `POST /api/calendars/{id}/links`: a new secret link, shown once.
pub async fn create_link(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(b): Json<LinkBody>,
) -> ApiResult<Value> {
    let detail = match b.detail.as_str() {
        "full" => Detail::Full,
        "busy" => Detail::Busy,
        _ => return Err(ApiError::bad("Detail is full or busy.")),
    };
    let (link, token) = links::create_link(&st.pool, u.id(), id, &b.label, detail).await?;
    let url = format!("{}/feeds/{token}.ics", super::base_url(&st, &headers));
    let webcal = match url.split_once("://") {
        Some((_, rest)) => format!("webcal://{rest}"),
        None => url.clone(),
    };
    Ok(Json(json!({ "id": link.id, "url": url, "webcal": webcal })))
}

/// `POST /api/links/{id}/revoke`: it stops working at once.
pub async fn revoke_link(State(st): State<AppState>, ApiUser(u): ApiUser, Path(id): Path<Uuid>) -> ApiResult<Value> {
    links::revoke(&st.pool, u.id(), id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
pub struct SubscribeBody {
    url: String,
    #[serde(default)]
    name: String,
    color: Option<String>,
    interval: Option<i32>,
}

/// `POST /api/subscriptions`: fetch the address first, and make the
/// calendar only from a copy that read completely, so a wrong address
/// never leaves an empty calendar behind.
pub async fn subscribe(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    headers: HeaderMap,
    Json(b): Json<SubscribeBody>,
) -> ApiResult<Value> {
    let lang = lang_of(&st, u.id(), &headers).await?;
    let fail = |m: String| ApiError(StatusCode::BAD_REQUEST, i18n::feed_error(lang, &m));
    let interval = b.interval.unwrap_or(60);
    if !INTERVALS.contains(&interval) {
        return Err(ApiError::bad("An interval of 15, 60, 360 or 1440 minutes."));
    }
    let url = feeds::check_url(&b.url).map_err(fail)?;
    let (bytes, etag, last_modified) =
        match feeds::fetch(&url, st.feeds_allow_private, None, None).await.map_err(fail)? {
            Fetched::Body { bytes, etag, last_modified } => (bytes, etag, last_modified),
            Fetched::NotModified => return Err(fail("The address answered with HTTP 304.".into())),
        };
    let (feed_name, objects) = feeds::read_feed(&bytes).map_err(fail)?;
    if objects.is_empty() {
        return Err(fail("The feed has no events.".into()));
    }
    let name = [b.name.trim(), feed_name.as_deref().unwrap_or("").trim(), url.host_str().unwrap_or("")]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("Subscribed")
        .chars()
        .take(100)
        .collect::<String>();
    let color = b.color.as_deref().filter(|c| !c.is_empty());
    let c = subscriptions::create(&st.pool, u.id(), url.as_str(), &name, color, interval).await?;
    match subscriptions::replace(&st.pool, c.id, &objects).await {
        Ok(r) => {
            subscriptions::record_success(&st.pool, c.id, etag.as_deref(), last_modified.as_deref()).await?;
            Ok(Json(json!({ "id": c.id, "added": r.added })))
        }
        Err(e) => {
            // Nothing half made: the new calendar goes again.
            let _ = collections::delete(&st.pool, u.id(), c.id).await;
            match e {
                DomainError::Invalid(why) => Err(fail(capitalized(&why))),
                other => Err(other.into()),
            }
        }
    }
}

fn capitalized(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str() + if s.ends_with('.') { "" } else { "." },
        None => String::new(),
    }
}

/// A subscribed calendar the person may manage (rename, refresh, delete).
async fn managed(st: &AppState, user: Uuid, id: Uuid) -> Result<subscriptions::Subscription, ApiError> {
    let c = collections::get(&st.pool, user, id).await?;
    if !c.access.can_admin() {
        return Err(DomainError::ReadOnly.into());
    }
    Ok(subscriptions::get(&st.pool, user, id).await?)
}

/// `POST /api/subscriptions/{id}/refresh`: now, and wait for it.
pub async fn refresh(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> ApiResult<Value> {
    let sub = managed(&st, u.id(), id).await?;
    let lang = lang_of(&st, u.id(), &headers).await?;
    match feeds::refresh(&st.pool, &sub, st.feeds_allow_private).await {
        Outcome::Unchanged => Ok(Json(json!({ "changed": false }))),
        Outcome::Updated(r) => Ok(Json(json!({
            "changed": r.added + r.changed + r.removed > 0,
            "added": r.added, "updated": r.changed, "removed": r.removed,
        }))),
        Outcome::Failed(why) => Err(ApiError(StatusCode::UNPROCESSABLE_ENTITY, i18n::feed_error(lang, &why))),
    }
}

#[derive(Deserialize)]
pub struct IntervalBody {
    minutes: i32,
}

/// `POST /api/subscriptions/{id}/interval`.
pub async fn set_interval(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    Path(id): Path<Uuid>,
    Json(b): Json<IntervalBody>,
) -> ApiResult<Value> {
    subscriptions::set_interval(&st.pool, u.id(), id, b.minutes).await?;
    Ok(Json(json!({ "ok": true })))
}

/// `POST /api/subscriptions/{id}/delete`: the calendar and its copy go.
pub async fn unsubscribe(State(st): State<AppState>, ApiUser(u): ApiUser, Path(id): Path<Uuid>) -> ApiResult<Value> {
    managed(&st, u.id(), id).await?;
    collections::delete(&st.pool, u.id(), id).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Template)]
#[template(path = "calendar_subscribe.html")]
struct SubscribeSheet {
    t: &'static i18n::T,
    intervals: Vec<(i32, &'static str)>,
}

/// The refresh choices in the person's words.
pub fn interval_options(t: &'static i18n::T) -> Vec<(i32, &'static str)> {
    INTERVALS
        .iter()
        .map(|m| {
            (
                *m,
                match m {
                    15 => t.every_15m,
                    60 => t.every_1h,
                    360 => t.every_6h,
                    _ => t.every_1d,
                },
            )
        })
        .collect()
}

/// `GET /calendar/subscribe`: the side sheet with the address form.
pub async fn subscribe_sheet(user: WebUser) -> Page {
    let t = i18n::t(user.lang);
    html(SubscribeSheet { t, intervals: interval_options(t) })
}
