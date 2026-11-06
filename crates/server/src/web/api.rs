//! The JSON endpoints the calendar page uses (phase 2 step 2): occurrences
//! for the grid, event writes with the recurring scope, undo, quick add,
//! personal settings and per-calendar settings. Same session, same Origin
//! guard and CSP as every page; errors are `{"error": "..."}` with a status.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use calzone_core::edit::Scope;
use calzone_core::moment::Key;
use calzone_core::quickadd::{self, QuickWhen};
use chrono::{NaiveDate, TimeZone, Utc};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use uuid::Uuid;

use super::i18n;
use super::session::ApiUser;
use crate::AppState;
use crate::domain::events::{self, EventInput, GridEvent, Saved, TimeInput};
use crate::domain::prefs::{self, CalendarPrefs, SettingsChange};
use crate::domain::{CollectionKind, DomainError, collections};

/// The widest range one request may ask for.
const MAX_RANGE_DAYS: i64 = 400;

pub struct ApiError(StatusCode, String);

impl ApiError {
    fn bad(msg: impl Into<String>) -> Self {
        ApiError(StatusCode::BAD_REQUEST, msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({ "error": self.1 }))).into_response()
    }
}

impl From<DomainError> for ApiError {
    fn from(e: DomainError) -> Self {
        match e {
            DomainError::NotFound => ApiError(StatusCode::NOT_FOUND, "Not found.".into()),
            DomainError::ReadOnly => ApiError(StatusCode::FORBIDDEN, "You can't change this calendar.".into()),
            DomainError::PreconditionFailed => {
                ApiError(StatusCode::PRECONDITION_FAILED, "It was changed somewhere else in the meantime.".into())
            }
            DomainError::Db(e) => {
                tracing::error!(error = %e, "api: database error");
                ApiError(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server.".into())
            }
            other => ApiError(StatusCode::BAD_REQUEST, other.to_string()),
        }
    }
}

type ApiResult<T> = Result<Json<T>, ApiError>;

/// The viewer's zone: their setting, else the server default.
async fn viewer(st: &AppState, user: uuid::Uuid) -> Result<(chrono_tz::Tz, prefs::Settings), ApiError> {
    let s = prefs::settings(&st.pool, user).await?;
    let tz = s.timezone.as_deref().and_then(|t| t.parse().ok()).unwrap_or(st.default_timezone);
    Ok((tz, s))
}

fn now() -> chrono::NaiveDateTime {
    Utc::now().naive_utc()
}

#[derive(Deserialize)]
pub struct RangeQuery {
    from: String,
    to: String,
}

/// `GET /api/occurrences?from=2026-01-05&to=2026-02-09`: days in the
/// viewer's zone, `to` exclusive.
pub async fn occurrences(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    Query(q): Query<RangeQuery>,
) -> ApiResult<Vec<GridEvent>> {
    let day = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").map_err(|_| ApiError::bad("Dates like 2026-01-05."));
    let (from, to) = (day(&q.from)?, day(&q.to)?);
    if to <= from || (to - from).num_days() > MAX_RANGE_DAYS {
        return Err(ApiError::bad(format!("A range of 1 to {MAX_RANGE_DAYS} days.")));
    }
    let (tz, _) = viewer(&st, u.id()).await?;
    let utc = |d: NaiveDate| {
        let local = d.and_hms_opt(0, 0, 0).expect("midnight");
        tz.from_local_datetime(&local).earliest().map(|t| t.naive_utc()).unwrap_or(local)
    };
    Ok(Json(events::occurrences(&st.pool, u.id(), utc(from), utc(to), tz).await?))
}

/// `absent` / `null` / value, for fields where null means "clear".
fn tri<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(d).map(Some)
}

#[derive(Deserialize, Default)]
pub struct ChangeBody {
    summary: Option<String>,
    location: Option<String>,
    description: Option<String>,
    start: Option<String>,
    end: Option<String>,
    #[serde(default, deserialize_with = "tri")]
    rrule: Option<Option<String>>,
    alarms: Option<Vec<i64>>,
}

impl ChangeBody {
    fn input(&self) -> Result<EventInput, ApiError> {
        let time = |v: &Option<String>, what: &str| -> Result<Option<TimeInput>, ApiError> {
            v.as_deref()
                .map(|s| s.parse().map_err(|_| ApiError::bad(format!("The {what} is not a date or time."))))
                .transpose()
        };
        if self.alarms.as_ref().is_some_and(|a| a.len() > 10 || a.iter().any(|m| !(-1440..=40_320).contains(m))) {
            return Err(ApiError::bad("Up to 10 reminders, up to four weeks before."));
        }
        Ok(EventInput {
            summary: self.summary.clone(),
            location: self.location.clone(),
            description: self.description.clone(),
            start: time(&self.start, "start")?,
            end: time(&self.end, "end")?,
            rrule: self.rrule.clone(),
            alarms: self.alarms.clone(),
        })
    }
}

#[derive(Deserialize)]
pub struct CreateBody {
    collection: Uuid,
    #[serde(flatten)]
    change: ChangeBody,
}

/// `POST /api/events`: a new event.
pub async fn create(State(st): State<AppState>, ApiUser(u): ApiUser, Json(b): Json<CreateBody>) -> ApiResult<Saved> {
    let (tz, _) = viewer(&st, u.id()).await?;
    Ok(Json(events::create(&st.pool, u.id(), b.collection, &b.change.input()?, tz, now()).await?))
}

fn scope(s: Option<&str>) -> Result<Scope, ApiError> {
    match s.unwrap_or("all") {
        "this" => Ok(Scope::This),
        "following" => Ok(Scope::Following),
        "all" => Ok(Scope::All),
        other => Err(ApiError::bad(format!("Unknown scope {other:?}."))),
    }
}

fn instance(s: Option<&str>) -> Result<Option<Key>, ApiError> {
    s.map(|k| k.parse::<Key>().map_err(|_| ApiError::bad("Not an occurrence."))).transpose()
}

#[derive(Deserialize)]
pub struct UpdateBody {
    etag: String,
    instance: Option<String>,
    scope: Option<String>,
    #[serde(flatten)]
    change: ChangeBody,
}

/// `POST /api/events/{collection}/{name}`: change it (or occurrences of it).
pub async fn update(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    Path((collection, name)): Path<(Uuid, String)>,
    Json(b): Json<UpdateBody>,
) -> ApiResult<Saved> {
    let (tz, _) = viewer(&st, u.id()).await?;
    let saved = events::update(
        &st.pool,
        u.id(),
        collection,
        &name,
        &b.etag,
        instance(b.instance.as_deref())?,
        scope(b.scope.as_deref())?,
        &b.change.input()?,
        tz,
        now(),
    )
    .await?;
    Ok(Json(saved))
}

#[derive(Deserialize)]
pub struct DeleteBody {
    etag: String,
    instance: Option<String>,
    scope: Option<String>,
}

/// `POST /api/events/{collection}/{name}/delete`.
pub async fn delete(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    Path((collection, name)): Path<(Uuid, String)>,
    Json(b): Json<DeleteBody>,
) -> ApiResult<Saved> {
    let saved = events::remove(
        &st.pool,
        u.id(),
        collection,
        &name,
        &b.etag,
        instance(b.instance.as_deref())?,
        scope(b.scope.as_deref())?,
        now(),
    )
    .await?;
    Ok(Json(saved))
}

/// `POST /api/undo/{change}`.
pub async fn undo(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    Path(change): Path<Uuid>,
) -> Result<Response, ApiError> {
    match events::undo(&st.pool, u.id(), change).await {
        Ok(()) => Ok(Json(json!({ "ok": true })).into_response()),
        Err(DomainError::PreconditionFailed) => {
            Err(ApiError(StatusCode::CONFLICT, "It was changed again since, so it can't be undone.".into()))
        }
        Err(DomainError::NotFound) => Err(ApiError(StatusCode::NOT_FOUND, "Nothing to undo.".into())),
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
pub struct QuickBody {
    text: String,
}

#[derive(Serialize, PartialEq, Debug)]
pub struct QuickDraft {
    title: String,
    all_day: bool,
    /// Viewer-local, like the grid's; `None` when no day or time was found.
    start: Option<String>,
    end: Option<String>,
}

/// `POST /api/quickadd`: "dentist thu 3pm" read in the person's language
/// and zone. Only reads; the page creates the event with `POST
/// /api/events` (or opens the form when there is no time).
pub async fn quickadd(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    headers: HeaderMap,
    Json(b): Json<QuickBody>,
) -> ApiResult<QuickDraft> {
    if b.text.chars().count() > 500 {
        return Err(ApiError::bad("Keep it under 500 characters."));
    }
    let (tz, s) = viewer(&st, u.id()).await?;
    let lang = i18n::lang(s.language.as_deref(), &headers);
    let local_now = tz.from_utc_datetime(&now()).naive_local();
    let d = quickadd::parse(&b.text, lang, local_now).ok_or_else(|| ApiError::bad("Add a title."))?;
    let (all_day, start, end) = match d.when {
        None => (false, None, None),
        Some(QuickWhen::Days { start, end }) => {
            (true, Some(start.format("%Y-%m-%d").to_string()), Some(end.format("%Y-%m-%d").to_string()))
        }
        Some(QuickWhen::Time { start, end }) => (
            false,
            Some(start.format("%Y-%m-%dT%H:%M:%S").to_string()),
            Some(end.format("%Y-%m-%dT%H:%M:%S").to_string()),
        ),
    };
    Ok(Json(QuickDraft { title: d.title, all_day, start, end }))
}

#[derive(Serialize)]
pub struct SettingsView {
    timezone: String,
    /// Whether the time zone was set (else it is the server default).
    timezone_set: bool,
    language: String,
    language_set: bool,
    theme: String,
    week_start: i16,
    clock_24h: bool,
}

/// `GET /api/settings`.
pub async fn settings(State(st): State<AppState>, ApiUser(u): ApiUser, headers: HeaderMap) -> ApiResult<SettingsView> {
    let (tz, s) = viewer(&st, u.id()).await?;
    let lang = match i18n::lang(s.language.as_deref(), &headers) {
        quickadd::Lang::En => "en",
        quickadd::Lang::Nl => "nl",
    };
    Ok(Json(SettingsView {
        timezone: tz.name().to_string(),
        timezone_set: s.timezone.is_some(),
        language: lang.to_string(),
        language_set: s.language.is_some(),
        theme: s.theme,
        week_start: s.week_start,
        clock_24h: s.clock_24h,
    }))
}

#[derive(Deserialize)]
pub struct SettingsBody {
    timezone: Option<String>,
    #[serde(default, deserialize_with = "tri")]
    language: Option<Option<String>>,
    theme: Option<String>,
    week_start: Option<i16>,
    clock_24h: Option<bool>,
}

/// `POST /api/settings`: any subset. `"language": null` follows the browser.
pub async fn save_settings(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    Json(b): Json<SettingsBody>,
) -> ApiResult<serde_json::Value> {
    let c = SettingsChange {
        timezone: b.timezone,
        language: b.language,
        theme: b.theme,
        week_start: b.week_start,
        clock_24h: b.clock_24h,
    };
    prefs::update_settings(&st.pool, u.id(), &c).await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Serialize)]
pub struct CalendarView {
    id: Uuid,
    /// The person's own name for it, else the calendar's.
    name: String,
    color: String,
    own: bool,
    owner: String,
    writable: bool,
    hidden: bool,
    sort_order: Option<i32>,
    default_alarm: Option<i32>,
}

/// `GET /api/calendars`: every calendar the person can see, with their own
/// settings, in their order (then by name).
pub async fn calendars(State(st): State<AppState>, ApiUser(u): ApiUser) -> ApiResult<Vec<CalendarView>> {
    let prefs = prefs::calendar_prefs(&st.pool, u.id()).await?;
    let mut out: Vec<CalendarView> = collections::visible(&st.pool, u.id(), CollectionKind::Calendar)
        .await?
        .into_iter()
        .map(|c| {
            let p = prefs.get(&c.id).cloned().unwrap_or_default();
            let own_name = if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() };
            CalendarView {
                id: c.id,
                name: p.display_name.unwrap_or(own_name),
                color: p.color.or(c.color).unwrap_or_else(|| events::DEFAULT_COLOR.into()),
                own: c.owner_id == u.id(),
                owner: c.owner_name,
                writable: c.access.can_write(),
                hidden: p.hidden,
                sort_order: p.sort_order,
                default_alarm: p.default_alarm,
            }
        })
        .collect();
    out.sort_by(|a, b| {
        a.sort_order
            .unwrap_or(i32::MAX)
            .cmp(&b.sort_order.unwrap_or(i32::MAX))
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    Ok(Json(out))
}

#[derive(Deserialize)]
pub struct PrefsBody {
    color: Option<String>,
    display_name: Option<String>,
    #[serde(default)]
    hidden: bool,
    sort_order: Option<i32>,
    default_alarm: Option<i32>,
}

/// `POST /api/calendars/{id}/prefs`: replaces the person's settings for it.
pub async fn save_prefs(
    State(st): State<AppState>,
    ApiUser(u): ApiUser,
    Path(id): Path<Uuid>,
    Json(b): Json<PrefsBody>,
) -> ApiResult<serde_json::Value> {
    let p = CalendarPrefs {
        color: b.color,
        display_name: b.display_name,
        hidden: b.hidden,
        sort_order: b.sort_order,
        default_alarm: b.default_alarm,
    };
    prefs::set_calendar_prefs(&st.pool, u.id(), id, &p).await?;
    Ok(Json(json!({ "ok": true })))
}
