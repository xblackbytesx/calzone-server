//! The calendar page (phase 2 step 3): the page around the EventCalendar
//! grid (decision 26), and the event panel, an HTML fragment the page's
//! script loads into a sheet. Writes go through `/api/` (decision 37).

use askama::Template;
use axum::extract::{Query, State};
use calzone_core::moment::Key;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::session::WebUser;
use super::{Nav, Page, WebError, html, i18n};
use crate::AppState;
use crate::domain::events::{self, ALARM_PRESETS, EventDetail};
use crate::domain::prefs;
use crate::domain::{CollectionKind, collections};

#[derive(Serialize)]
struct Cal {
    id: Uuid,
    name: String,
    color: String,
}

/// What the script needs besides the strings.
#[derive(Serialize)]
struct Config {
    locale: &'static str,
    /// For the grid: 0 = Sunday ... 6 = Saturday.
    first_day: u8,
    hour12: bool,
    timezone_set: bool,
    /// Calendars the person can add to, in their order: quick add uses the
    /// first.
    calendars: Vec<Cal>,
}

/// The person's calendars they can write to, visible, in their order.
async fn writable(st: &AppState, user: &WebUser) -> Result<Vec<Cal>, WebError> {
    let p = prefs::calendar_prefs(&st.pool, user.id()).await?;
    let mut out: Vec<(i32, bool, Cal)> = collections::visible(&st.pool, user.id(), CollectionKind::Calendar)
        .await?
        .into_iter()
        .filter(|c| c.access.can_write())
        .filter_map(|c| {
            let pr = p.get(&c.id).cloned().unwrap_or_default();
            if pr.hidden {
                return None;
            }
            let base = if c.display_name.is_empty() { c.slug.clone() } else { c.display_name.clone() };
            Some((
                pr.sort_order.unwrap_or(i32::MAX),
                c.owner_id != user.id(),
                Cal {
                    id: c.id,
                    name: pr.display_name.unwrap_or(base),
                    color: pr.color.or(c.color).unwrap_or_else(|| events::DEFAULT_COLOR.into()),
                },
            ))
        })
        .collect();
    out.sort_by(|a, b| (a.0, a.1, a.2.name.to_lowercase()).cmp(&(b.0, b.1, b.2.name.to_lowercase())));
    Ok(out.into_iter().map(|x| x.2).collect())
}

#[derive(Template)]
#[template(path = "calendar.html")]
struct CalendarPage {
    nav: Nav,
    strings: String,
    config: String,
    has_calendar: bool,
}

pub async fn page(State(st): State<AppState>, user: WebUser) -> Page {
    let s = prefs::settings(&st.pool, user.id()).await?;
    let calendars = writable(&st, &user).await?;
    let config = Config {
        locale: i18n::locale(user.lang),
        first_day: (s.week_start % 7) as u8,
        hour12: !s.clock_24h,
        timezone_set: s.timezone.is_some(),
        calendars,
    };
    let has_calendar = !config.calendars.is_empty();
    html(CalendarPage {
        nav: Nav::new(&st, &user, "calendar"),
        strings: serde_json::to_string(i18n::t(user.lang)).expect("strings serialize"),
        config: serde_json::to_string(&config).expect("config serializes"),
        has_calendar,
    })
}

/// The values the panel starts with; the script sends only what changed.
#[derive(Serialize)]
struct Initial<'a> {
    title: &'a str,
    all_day: bool,
    start_date: &'a str,
    start_time: &'a str,
    end_date: &'a str,
    end_time: &'a str,
    repeat: &'a str,
    location: &'a str,
    notes: &'a str,
    alarm: String,
}

#[derive(Template)]
#[template(path = "event_panel.html")]
struct PanelPage {
    t: &'static i18n::T,
    new: bool,
    d: EventDetail,
    initial: String,
    calendars: Vec<Cal>,
    alarms: Vec<(String, &'static str)>,
    alarm_value: String,
}

fn alarm_value(d: &EventDetail) -> String {
    match (d.alarm, d.alarm_kept) {
        (_, true) => "keep".into(),
        (Some(m), _) => m.to_string(),
        (None, _) => "none".into(),
    }
}

fn alarm_options(t: &'static i18n::T) -> Vec<(String, &'static str)> {
    let labels = [t.reminder_at_start, t.reminder_5m, t.reminder_15m, t.reminder_30m, t.reminder_1h, t.reminder_1d];
    ALARM_PRESETS.iter().zip(labels).map(|(m, l)| (m.to_string(), l)).collect()
}

fn panel(user: &WebUser, new: bool, d: EventDetail, calendars: Vec<Cal>) -> Page {
    let t = i18n::t(user.lang);
    let alarm = alarm_value(&d);
    let initial = Initial {
        title: &d.title,
        all_day: d.all_day,
        start_date: &d.start_date,
        start_time: &d.start_time,
        end_date: &d.end_date,
        end_time: &d.end_time,
        repeat: d.repeat,
        location: &d.location,
        notes: &d.description,
        alarm: alarm.clone(),
    };
    let initial = serde_json::to_string(&initial).expect("serializes");
    html(PanelPage { t, new, d, initial, calendars, alarms: alarm_options(t), alarm_value: alarm })
}

#[derive(Deserialize)]
pub struct EventQuery {
    c: Uuid,
    n: String,
    i: Option<String>,
}

/// `GET /calendar/event?c=&n=&i=`: the panel for an event or occurrence.
pub async fn event(State(st): State<AppState>, user: WebUser, Query(q): Query<EventQuery>) -> Page {
    let instance =
        q.i.as_deref()
            .filter(|s| !s.is_empty())
            .map(|s| s.parse::<Key>())
            .transpose()
            .map_err(|_| WebError::bad("Not an occurrence."))?;
    let tz = viewer_tz(&st, &user).await?;
    let d = events::detail(&st.pool, user.id(), q.c, &q.n, instance, tz).await?;
    panel(&user, false, d, Vec::new())
}

async fn viewer_tz(st: &AppState, user: &WebUser) -> Result<chrono_tz::Tz, WebError> {
    let s = prefs::settings(&st.pool, user.id()).await?;
    Ok(s.timezone.as_deref().and_then(|t| t.parse().ok()).unwrap_or(st.default_timezone))
}

#[derive(Deserialize)]
pub struct NewQuery {
    start: Option<String>,
    end: Option<String>,
    all_day: Option<String>,
    title: Option<String>,
    c: Option<Uuid>,
}

/// `GET /calendar/event/new?start=&end=&all_day=1&title=`: the panel for a
/// new event, times viewer-local like the grid's.
pub async fn new_event(State(st): State<AppState>, user: WebUser, Query(q): Query<NewQuery>) -> Page {
    let mut calendars = writable(&st, &user).await?;
    if let Some(c) = q.c
        && let Some(i) = calendars.iter().position(|x| x.id == c)
    {
        let chosen = calendars.remove(i);
        calendars.insert(0, chosen);
    }
    let first = calendars.first().ok_or_else(|| WebError::bad(i18n::t(user.lang).no_calendar))?;
    let tz = viewer_tz(&st, &user).await?;
    let now = chrono::Utc::now().with_timezone(&tz).naive_local();
    let parse = |s: &Option<String>| s.as_deref().and_then(|v| v.parse::<events::TimeInput>().ok());
    let all_day = q.all_day.as_deref() == Some("1");
    let (start, end) = match (parse(&q.start), parse(&q.end)) {
        (Some(s), e) => (s, e),
        (None, _) => {
            // The next whole hour.
            let h =
                now.date().and_hms_opt(now.time().format("%H").to_string().parse().unwrap_or(9), 0, 0).expect("hour")
                    + chrono::Duration::hours(1);
            (events::TimeInput::Local(h), None)
        }
    };
    let (sd, st_, all_day) = match start {
        events::TimeInput::Date(d) => (d, None, true),
        events::TimeInput::Local(t) if all_day => (t.date(), None, true),
        events::TimeInput::Local(t) => (t.date(), Some(t.time()), false),
    };
    let (ed, et) = match (end, st_) {
        (Some(events::TimeInput::Date(d)), _) => ((d - chrono::Duration::days(1)).max(sd), None),
        (Some(events::TimeInput::Local(t)), Some(_)) => (t.date(), Some(t.time())),
        (_, Some(s)) => {
            let e = sd.and_time(s) + chrono::Duration::hours(1);
            (e.date(), Some(e.time()))
        }
        (_, None) => (sd, None),
    };
    let fmt_t = |t: Option<chrono::NaiveTime>| t.map(|t| t.format("%H:%M").to_string()).unwrap_or_default();
    let d = EventDetail {
        collection: first.id,
        name: String::new(),
        etag: String::new(),
        instance: None,
        title: q.title.unwrap_or_default().chars().take(500).collect(),
        location: String::new(),
        description: String::new(),
        all_day,
        start_date: sd.format("%Y-%m-%d").to_string(),
        start_time: fmt_t(st_).if_empty("09:00"),
        end_date: ed.format("%Y-%m-%d").to_string(),
        end_time: fmt_t(et).if_empty("10:00"),
        repeat: "none",
        recurring: false,
        alarm: None,
        alarm_kept: false,
        calendar: first.name.clone(),
        owner: user.account.name.clone(),
        own: true,
        editable: true,
        color: first.color.clone(),
    };
    panel(&user, true, d, calendars)
}

trait IfEmpty {
    fn if_empty(self, default: &str) -> String;
}

impl IfEmpty for String {
    fn if_empty(self, default: &str) -> String {
        if self.is_empty() { default.to_string() } else { self }
    }
}
