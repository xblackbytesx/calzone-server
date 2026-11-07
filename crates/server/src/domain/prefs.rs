//! Personal settings (time zone, language, theme, week start, clock) and how
//! each person sees each calendar (decision 8): their own color, name,
//! visibility, order and default reminder. Shared data never changes here.

use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use super::{Actor, DomainError, Result, access};

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Settings {
    /// IANA zone; None until set.
    pub timezone: Option<String>,
    /// `en` or `nl`; None follows the browser.
    pub language: Option<String>,
    /// `dark`, `light` or `system`.
    pub theme: String,
    /// 1 = Monday ... 7 = Sunday.
    pub week_start: i16,
    pub clock_24h: bool,
}

/// What to change; `None` leaves a field alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SettingsChange {
    pub timezone: Option<String>,
    /// `Some(None)` goes back to following the browser.
    pub language: Option<Option<String>>,
    pub theme: Option<String>,
    pub week_start: Option<i16>,
    pub clock_24h: Option<bool>,
}

pub async fn settings(pool: &PgPool, user: Actor) -> Result<Settings> {
    let row: Option<Settings> =
        sqlx::query_as("SELECT timezone, language, theme, week_start, clock_24h FROM principals WHERE id = $1")
            .bind(user)
            .fetch_optional(pool)
            .await?;
    row.ok_or(DomainError::NotFound)
}

pub fn valid_timezone(tz: &str) -> bool {
    tz.parse::<chrono_tz::Tz>().is_ok()
}

pub async fn update_settings(pool: &PgPool, user: Actor, c: &SettingsChange) -> Result<()> {
    let bad = |what: &str| DomainError::Invalid(what.to_string());
    if c.timezone.as_deref().is_some_and(|t| !valid_timezone(t)) {
        return Err(bad("an unknown time zone"));
    }
    if c.language.as_ref().is_some_and(|l| l.as_deref().is_some_and(|l| !matches!(l, "en" | "nl"))) {
        return Err(bad("the language must be en or nl"));
    }
    if c.theme.as_deref().is_some_and(|t| !matches!(t, "dark" | "light" | "system")) {
        return Err(bad("the theme must be dark, light or system"));
    }
    if c.week_start.is_some_and(|w| !(1..=7).contains(&w)) {
        return Err(bad("the week starts on day 1 to 7"));
    }
    let r = sqlx::query(
        "UPDATE principals SET
            timezone = COALESCE($2, timezone),
            language = CASE WHEN $3 THEN $4 ELSE language END,
            theme = COALESCE($5, theme),
            week_start = COALESCE($6, week_start),
            clock_24h = COALESCE($7, clock_24h)
         WHERE id = $1 AND kind = 'user'",
    )
    .bind(user)
    .bind(&c.timezone)
    .bind(c.language.is_some())
    .bind(c.language.clone().flatten())
    .bind(&c.theme)
    .bind(c.week_start)
    .bind(c.clock_24h)
    .execute(pool)
    .await?;
    if r.rows_affected() == 0 {
        return Err(DomainError::NotFound);
    }
    Ok(())
}

/// One person's view of one calendar.
#[derive(Debug, Clone, Default, PartialEq, Eq, sqlx::FromRow)]
pub struct CalendarPrefs {
    pub color: Option<String>,
    pub display_name: Option<String>,
    pub hidden: bool,
    pub sort_order: Option<i32>,
    /// Minutes before the start.
    pub default_alarm: Option<i32>,
}

/// The actor's prefs for every collection they have set any for.
pub async fn calendar_prefs(pool: &PgPool, actor: Actor) -> Result<HashMap<Uuid, CalendarPrefs>> {
    #[derive(sqlx::FromRow)]
    struct Row {
        collection_id: Uuid,
        #[sqlx(flatten)]
        prefs: CalendarPrefs,
    }
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT collection_id, color, display_name, hidden, sort_order, default_alarm
         FROM collection_prefs WHERE principal_id = $1",
    )
    .bind(actor)
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(|r| (r.collection_id, r.prefs)).collect())
}

fn valid_color(c: &str) -> bool {
    c.len() == 7 && c.starts_with('#') && c[1..].bytes().all(|b| b.is_ascii_hexdigit())
}

/// Replace the actor's prefs for a collection they can see.
pub async fn set_calendar_prefs(pool: &PgPool, actor: Actor, collection: Uuid, p: &CalendarPrefs) -> Result<()> {
    access::of(pool, actor, collection).await?.ok_or(DomainError::NotFound)?;
    if p.color.as_deref().is_some_and(|c| !valid_color(c)) {
        return Err(DomainError::Invalid("a color like #3a7bd5".into()));
    }
    if p.display_name.as_deref().is_some_and(|n| n.chars().count() > 100) {
        return Err(DomainError::Invalid("a name of at most 100 characters".into()));
    }
    if p.default_alarm.is_some_and(|m| !(0..=40_320).contains(&m)) {
        return Err(DomainError::Invalid("a reminder up to four weeks before".into()));
    }
    let name = p.display_name.as_deref().map(str::trim).filter(|n| !n.is_empty());
    sqlx::query(
        "INSERT INTO collection_prefs (principal_id, collection_id, color, display_name, hidden, sort_order, default_alarm)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (principal_id, collection_id) DO UPDATE SET
            color = EXCLUDED.color, display_name = EXCLUDED.display_name, hidden = EXCLUDED.hidden,
            sort_order = EXCLUDED.sort_order, default_alarm = EXCLUDED.default_alarm",
    )
    .bind(actor)
    .bind(collection)
    .bind(p.color.as_deref().map(str::to_ascii_lowercase))
    .bind(name)
    .bind(p.hidden)
    .bind(p.sort_order)
    .bind(p.default_alarm)
    .execute(pool)
    .await?;
    Ok(())
}
