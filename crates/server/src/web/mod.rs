//! The web app's admin pages (phase 1 step 5): server-rendered askama
//! pages, enhanced with HTMX (decision 11). The calendar itself arrives in
//! phase 2.
//!
//! No inline script or style anywhere (the CSP forbids both); HTMX and the
//! small `app.js` are served from here, with HTMX's inline indicator styles
//! and eval-based features turned off in `base.html`. Every page calls the
//! domain service only.

mod admin;
mod api;
mod calendar;
mod collections;
mod contacts;
mod feeds;
pub mod i18n;
mod pages;
mod passwords;
pub mod session;
mod transfer;
mod two_factor;

use std::sync::LazyLock;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE, ETAG, HOST, IF_NONE_MATCH, ORIGIN};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use sha2::{Digest, Sha256};

use crate::AppState;
use crate::domain::DomainError;
use session::WebUser;

/// The web routes. With the DAV door on, `/` also answers PROPFIND (its
/// fallback), for clients that start at the bare server address.
pub fn routes(state: &AppState) -> Router<AppState> {
    let dav = state.dav;
    let mut root = get(pages::home);
    if dav {
        root = root.fallback(crate::dav::handle);
    }
    Router::new()
        .route("/", root)
        .route("/login", get(pages::login_page).post(pages::login))
        .route("/logout", post(pages::logout))
        .route("/settings", get(pages::settings))
        .route("/settings/profile", post(pages::save_profile))
        .route("/settings/passphrase", post(pages::change_passphrase))
        .route("/settings/preferences", post(pages::save_preferences))
        .route("/login/code", get(two_factor::code_page).post(two_factor::code))
        .route("/settings/two-factor", get(two_factor::setup).post(two_factor::confirm))
        .route("/settings/two-factor/recovery", post(two_factor::new_codes))
        .route("/settings/two-factor/off", post(two_factor::turn_off))
        .route("/collections", get(collections::list).post(collections::create))
        .route("/collections/{id}", get(collections::show).post(collections::update))
        .route("/collections/{id}/share", post(collections::share))
        .route("/collections/{id}/unshare/{principal}", post(collections::unshare))
        .route("/collections/{id}/delete", post(collections::delete))
        .route("/devices", get(passwords::list).post(passwords::create))
        .route("/devices/{id}/revoke", post(passwords::revoke))
        .route("/devices/link", post(passwords::create_link))
        .route("/devices/phones/{id}/remove", post(passwords::remove_phone))
        .route("/link/{code}", get(passwords::link_landing))
        .route("/admin/people", get(admin::people).post(admin::create_user))
        .route("/admin/people/{id}/passphrase", post(admin::reset_passphrase))
        .route("/admin/people/{id}/disable", post(admin::disable))
        .route("/admin/people/{id}/enable", post(admin::enable))
        .route("/admin/people/{id}/admin", post(admin::set_admin))
        .route("/admin/people/{id}/two-factor-reset", post(admin::reset_two_factor))
        .route("/admin/groups", post(admin::create_group))
        .route("/admin/groups/{id}/members", post(admin::add_member))
        .route("/admin/groups/{id}/members/{member}/remove", post(admin::remove_member))
        .route("/contacts", get(contacts::list))
        .route("/contacts/new", get(contacts::new_form).post(contacts::create))
        .route("/contacts/undo/{change}", post(contacts::undo))
        .route("/contacts/selected", post(contacts::selected))
        .route("/contacts/delete-many", post(contacts::delete_many))
        .route("/contacts/duplicates", get(contacts::duplicates))
        .route("/contacts/duplicates/apart", post(contacts::mark_apart))
        .route("/contacts/duplicates/together", post(contacts::unmark_apart))
        .route("/contacts/merge", get(contacts::merge_form).post(contacts::merge))
        .route("/contacts/photo/{c}/{n}", get(contacts::photo))
        .route("/contacts/{c}/{n}", get(contacts::show).post(contacts::update))
        .route("/contacts/{c}/{n}/edit", get(contacts::edit_form))
        .route("/contacts/{c}/{n}/delete", post(contacts::delete))
        .route("/calendar", get(calendar::page))
        .route("/calendar/event", get(calendar::event))
        .route("/calendar/event/new", get(calendar::new_event))
        .route("/calendar/settings/{id}", get(calendar::settings_sheet))
        .route("/static/event-calendar.min.js", get(|h: HeaderMap| asset(h, &EC_JS, "text/javascript")))
        .route("/static/event-calendar.min.css", get(|h: HeaderMap| asset(h, &EC_CSS, "text/css")))
        .route("/static/calendar.js", get(|h: HeaderMap| asset(h, &CAL_JS, "text/javascript")))
        .route("/api/occurrences", get(api::occurrences))
        .route("/api/events", post(api::create))
        .route("/api/events/{collection}/{name}", post(api::update))
        .route("/api/events/{collection}/{name}/delete", post(api::delete))
        .route("/api/undo/{change}", post(api::undo))
        .route("/api/quickadd", post(api::quickadd))
        .route("/api/settings", get(api::settings).post(api::save_settings))
        .route("/api/calendars", get(api::calendars))
        .route("/api/calendars/{id}/prefs", post(api::save_prefs))
        .route("/api/calendars/{id}/links", post(feeds::create_link))
        .route("/api/links/{id}/revoke", post(feeds::revoke_link))
        .route("/api/subscriptions", post(feeds::subscribe))
        .route("/api/subscriptions/{id}/refresh", post(feeds::refresh))
        .route("/api/subscriptions/{id}/interval", post(feeds::set_interval))
        .route("/api/subscriptions/{id}/delete", post(feeds::unsubscribe))
        .route("/calendar/subscribe", get(feeds::subscribe_sheet))
        .route("/feeds/{file}", get(feeds::feed))
        .route("/import", get(transfer::page))
        .route("/import/file", post(transfer::upload).layer(DefaultBodyLimit::max(transfer::UPLOAD_LIMIT)))
        .route("/import/baikal", post(transfer::baikal))
        .route("/import/{id}", get(transfer::report))
        .route("/import/{id}/failed/{n}", get(transfer::raw))
        .route("/export.zip", get(transfer::export_all))
        .route("/collections/{id}/export", get(transfer::export_one))
        .route("/static/app.css", get(|h: HeaderMap| asset(h, &CSS, "text/css")))
        .route("/static/app.js", get(|h: HeaderMap| asset(h, &JS, "text/javascript")))
        .route("/static/htmx.min.js", get(|h: HeaderMap| asset(h, &HTMX, "text/javascript")))
        .layer(DefaultBodyLimit::max(64 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), guard))
}

/// An embedded static file with its ETag, computed once.
pub struct Asset {
    body: &'static [u8],
    etag: String,
}

fn embed(body: &'static [u8]) -> Asset {
    let digest = Sha256::digest(body);
    let hex: String = digest.iter().take(8).map(|b| format!("{b:02x}")).collect();
    Asset { body, etag: format!("\"{hex}\"") }
}

static CSS: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/app.css")));
static JS: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/app.js")));
/// EventCalendar 5.18.1, vendored (decision 26). sha256 in AGENTS.md.
static EC_JS: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/event-calendar.min.js")));
static EC_CSS: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/event-calendar.min.css")));
static CAL_JS: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/calendar.js")));
/// htmx 2.0.4, vendored. sha256 in AGENTS.md.
static HTMX: LazyLock<Asset> = LazyLock::new(|| embed(include_bytes!("../../static/htmx.min.js")));

/// `no-cache` + ETag: a deploy is picked up on the next page view, and every
/// other view is a 304 with no body.
async fn asset(headers: HeaderMap, a: &Asset, content_type: &'static str) -> Response {
    let etag = HeaderValue::from_str(&a.etag).expect("hex etag");
    let common = [(CACHE_CONTROL, HeaderValue::from_static("no-cache")), (ETAG, etag)];
    if headers.get(IF_NONE_MATCH).is_some_and(|v| v.as_bytes() == a.etag.as_bytes()) {
        return (StatusCode::NOT_MODIFIED, common).into_response();
    }
    (common, [(CONTENT_TYPE, HeaderValue::from_static(content_type))], a.body).into_response()
}

/// Nothing external: every byte the pages use comes from this server.
pub const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; \
     connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'";

/// Refuses cross-site writes (an `Origin` must name this host when sent) and
/// sets the page security headers. `SameSite=Strict` cookies already stop
/// most CSRF; this is the second lock. Every method but GET and HEAD counts
/// as a write. DAV clients (PROPFIND on `/`) send no `Origin`.
///
/// It also finishes error fragments: a generic error (one a domain error
/// made, marked `x-cz-generic`) is put in the reader's language, and a
/// fragment that reached a plain browser request becomes a small page.
async fn guard(State(st): State<AppState>, req: Request, next: Next) -> Response {
    if !matches!(*req.method(), Method::GET | Method::HEAD)
        && let Some(origin) = req.headers().get(ORIGIN).and_then(|v| v.to_str().ok())
    {
        let host = req.headers().get(HOST).and_then(|v| v.to_str().ok()).unwrap_or("");
        let origin_host = origin.split("://").nth(1).unwrap_or("");
        if origin_host != host {
            return (StatusCode::FORBIDDEN, "cross-site request refused").into_response();
        }
    }
    let htmx = is_htmx(req.headers());
    let headers = req.headers().clone();
    let mut resp = next.run(req).await;
    let generic = resp.headers_mut().remove("x-cz-generic");
    let fragment = resp.headers().contains_key("hx-retarget");
    if generic.is_some() || (fragment && !htmx) {
        let lang = request_lang(&st, &headers).await;
        if let Some(key) = generic.as_ref().and_then(|v| v.to_str().ok())
            && let Some(msg) = i18n::a(lang).generic(key)
        {
            let status = resp.status();
            let (mut parts, _) = resp.into_parts();
            parts.headers.remove(axum::http::header::CONTENT_LENGTH);
            let body = format!("<p class=\"notice error\" role=\"alert\">{}</p>", escape(msg));
            resp = Response::from_parts(parts, axum::body::Body::from(body));
            *resp.status_mut() = status;
        }
        if fragment && !htmx {
            resp = error_page(resp, lang).await;
        }
    }
    let h = resp.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    // Not `no-referrer`: under it browsers send `Origin: null` on form posts,
    // which the check above refuses.
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    resp
}

/// The reader's language for a message the handler could not translate:
/// their setting when signed in (or half way), else the browser's.
async fn request_lang(st: &AppState, headers: &HeaderMap) -> calzone_core::quickadd::Lang {
    let token = headers
        .get_all(axum::http::header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == session::COOKIE_NAME)
        .map(|(_, v)| v.to_string());
    let mut setting = None;
    if let Some(t) = token {
        let account = match crate::domain::accounts::session_user(&st.pool, &t).await {
            Ok(Some(a)) => Some(a),
            _ => crate::domain::accounts::pending_user(&st.pool, &t).await.ok().flatten(),
        };
        setting = account.and_then(|a| a.language);
    }
    i18n::lang(setting.as_deref(), headers)
}

/// An error fragment reached a plain browser request (no JavaScript): wrap
/// it in a minimal page with a way back.
async fn error_page(resp: Response, lang: calzone_core::quickadd::Lang) -> Response {
    let a = i18n::a(lang);
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap_or_default();
    let fragment = String::from_utf8_lossy(&bytes);
    let page = format!(
        "<!doctype html><html lang=\"{}\" data-theme=\"dark\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width, initial-scale=1\"><meta name=\"color-scheme\" content=\"dark\"><title>CalZone</title><link rel=\"stylesheet\" \
         href=\"/static/app.css\"></head><body><main class=\"page auth\"><section class=\"panel\">{fragment}\
         <p>{} <a href=\"/\">{}</a>.</p></section></main></body></html>",
        i18n::code(lang),
        escape(a.back_or),
        escape(a.start_over)
    );
    (status, Html(page)).into_response()
}

/// A failed page action: an HTML snippet that HTMX swaps into the page's
/// `#flash` box (HX-Retarget), so a failed action never replaces a form.
#[derive(Debug)]
pub struct WebError {
    pub status: StatusCode,
    pub message: String,
    /// A generic error the guard translates (`not_found`, ...).
    pub key: Option<&'static str>,
}

impl WebError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into(), key: None }
    }

    pub fn bad(message: impl Into<String>) -> Self {
        Self::new(StatusCode::BAD_REQUEST, message)
    }
}

impl From<DomainError> for WebError {
    fn from(e: DomainError) -> Self {
        let (status, msg) = match e {
            DomainError::NotFound => (StatusCode::NOT_FOUND, "Not found.".to_string()),
            DomainError::ReadOnly => (StatusCode::FORBIDDEN, "You can't change this.".to_string()),
            DomainError::NameTaken => (StatusCode::CONFLICT, "That name is already taken.".to_string()),
            DomainError::BadName => (
                StatusCode::BAD_REQUEST,
                "Use 1 to 64 lowercase letters, digits, dots, dashes or underscores.".to_string(),
            ),
            DomainError::Invalid(m) => (StatusCode::BAD_REQUEST, capitalize(&m)),
            DomainError::Db(e) => {
                tracing::error!(error = %e, "web: database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server.".to_string())
            }
            other => (StatusCode::BAD_REQUEST, capitalize(&other.to_string())),
        };
        let key = match status {
            StatusCode::NOT_FOUND => Some("not_found"),
            StatusCode::FORBIDDEN => Some("read_only"),
            StatusCode::CONFLICT => Some("name_taken"),
            StatusCode::INTERNAL_SERVER_ERROR => Some("server"),
            _ if msg.starts_with("Use 1 to 64") => Some("bad_name"),
            _ => None,
        };
        WebError { status, message: msg, key }
    }
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str() + if s.ends_with('.') { "" } else { "." },
        None => String::new(),
    }
}

pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

impl IntoResponse for WebError {
    fn into_response(self) -> Response {
        let body = format!("<p class=\"notice error\" role=\"alert\">{}</p>", escape(&self.message));
        let mut r = (self.status, Html(body)).into_response();
        let h = r.headers_mut();
        h.insert("hx-retarget", HeaderValue::from_static("#flash"));
        h.insert("hx-reswap", HeaderValue::from_static("innerHTML"));
        if let Some(k) = self.key {
            h.insert("x-cz-generic", HeaderValue::from_static(k));
        }
        r
    }
}

pub type Page = Result<Response, WebError>;

/// What every signed-in page needs for the header and `<html>`.
pub struct Nav {
    pub label: String,
    pub admin: bool,
    pub dav: bool,
    pub active: &'static str,
    /// `en` or `nl` (decision 35).
    pub lang: &'static str,
    /// `dark`, `light` or `system` (decision 36).
    pub theme: &'static str,
    pub t: &'static i18n::T,
    /// The admin pages' strings.
    pub a: &'static i18n::A,
}

/// A stored theme as one of the four values the CSS knows.
pub fn theme(stored: &str) -> &'static str {
    match stored {
        "light" => "light",
        "black" => "black",
        "system" => "system",
        _ => "dark",
    }
}

impl Nav {
    pub fn new(st: &AppState, u: &WebUser, active: &'static str) -> Self {
        Nav {
            label: u.account.label().to_string(),
            admin: u.account.is_admin,
            dav: st.dav,
            active,
            lang: i18n::code(u.lang),
            theme: theme(&u.account.theme),
            t: i18n::t(u.lang),
            a: i18n::a(u.lang),
        }
    }

    /// `aria-current` for a section's link.
    pub fn on(&self, section: &str) -> &'static str {
        if self.active == section { "page" } else { "false" }
    }

    /// The gear lights up on every page behind Settings (decision 53).
    pub fn in_settings(&self) -> &'static str {
        if matches!(self.active, "settings" | "collections" | "devices" | "import" | "people") {
            "page"
        } else {
            "false"
        }
    }
}

pub fn html(t: impl askama::Template) -> Page {
    t.render().map(|s| Html(s).into_response()).map_err(|e| {
        tracing::error!(error = %e, "web: template");
        WebError::new(StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server.")
    })
}

pub fn is_htmx(headers: &HeaderMap) -> bool {
    headers.contains_key("hx-request")
}

/// This server as clients reach it: `CALZONE_PUBLIC_URL`, else the request's
/// host.
pub fn base_url(st: &AppState, headers: &HeaderMap) -> String {
    if let Some(u) = &st.public_url {
        return u.clone();
    }
    let host = headers.get(HOST).and_then(|v| v.to_str().ok()).unwrap_or("localhost");
    format!("{}://{host}", if st.cookie_secure { "https" } else { "http" })
}

/// A color for `<input type="color">` (`#rrggbb`), from what clients store
/// (`#rgb`, `#rrggbb`, `#rrggbbaa`).
pub fn color_input(c: Option<&str>) -> String {
    let Some(c) = c.and_then(|c| c.strip_prefix('#')) else { return "#3a7bd5".into() };
    if !c.bytes().all(|b| b.is_ascii_hexdigit()) {
        return "#3a7bd5".into();
    }
    match c.len() {
        3 => c.chars().flat_map(|x| [x, x]).fold("#".to_string(), |mut s, x| {
            s.push(x);
            s
        }),
        6 | 8 => format!("#{}", &c[..6].to_ascii_lowercase()),
        _ => "#3a7bd5".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_for_the_picker() {
        assert_eq!(color_input(Some("#3366CCFF")), "#3366cc");
        assert_eq!(color_input(Some("#abc")), "#aabbcc");
        assert_eq!(color_input(Some("red")), "#3a7bd5");
        assert_eq!(color_input(Some("#12345z")), "#3a7bd5");
        assert_eq!(color_input(None), "#3a7bd5");
    }

    /// The CSP forbids inline script and style; a template that used them
    /// would break silently in the browser, so catch it here.
    #[test]
    fn templates_have_no_inline_script_or_style() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("templates");
        let mut n = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let text = std::fs::read_to_string(&path).unwrap().to_ascii_lowercase();
            let name = path.display();
            assert!(!text.contains("style=") && !text.contains("<style"), "{name}: inline style");
            assert!(!text.contains("javascript:"), "{name}: javascript: URL");
            for (i, _) in text.match_indices("<script") {
                let tag = &text[i..i + text[i..].find('>').unwrap()];
                assert!(tag.contains(" src="), "{name}: inline script");
            }
            for attr in [" onclick=", " onsubmit=", " onload=", " onchange=", " oninput=", " onerror="] {
                assert!(!text.contains(attr), "{name}: inline handler {attr}");
            }
            n += 1;
        }
        assert!(n >= 10, "found the templates");
    }

    #[test]
    fn messages() {
        assert_eq!(capitalize("a label of 1 to 100 characters"), "A label of 1 to 100 characters.");
        assert_eq!(escape("<a href=\"x\">'"), "&lt;a href=&quot;x&quot;&gt;&#39;");
    }
}
