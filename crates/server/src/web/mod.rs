//! The web app's admin pages (phase 1 step 5): server-rendered askama
//! pages, enhanced with HTMX (decision 11). The calendar itself arrives in
//! phase 2.
//!
//! No inline script or style anywhere (the CSP forbids both); HTMX and the
//! small `app.js` are served from here, with HTMX's inline indicator styles
//! and eval-based features turned off in `base.html`. Every page calls the
//! domain service only.

mod admin;
mod collections;
mod pages;
mod passwords;
pub mod session;
mod transfer;
mod two_factor;

use std::sync::LazyLock;

use axum::Router;
use axum::extract::{DefaultBodyLimit, Request};
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
pub fn routes(dav: bool) -> Router<AppState> {
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
        .route("/admin/people", get(admin::people).post(admin::create_user))
        .route("/admin/people/{id}/passphrase", post(admin::reset_passphrase))
        .route("/admin/people/{id}/disable", post(admin::disable))
        .route("/admin/people/{id}/enable", post(admin::enable))
        .route("/admin/people/{id}/admin", post(admin::set_admin))
        .route("/admin/people/{id}/two-factor-reset", post(admin::reset_two_factor))
        .route("/admin/groups", post(admin::create_group))
        .route("/admin/groups/{id}/members", post(admin::add_member))
        .route("/admin/groups/{id}/members/{member}/remove", post(admin::remove_member))
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
        .layer(middleware::from_fn(guard))
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
async fn guard(req: Request, next: Next) -> Response {
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
    let mut resp = next.run(req).await;
    if !htmx && resp.headers().contains_key("hx-retarget") {
        resp = error_page(resp).await;
    }
    let h = resp.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    // Not `no-referrer`: under it browsers send `Origin: null` on form posts,
    // which the check above refuses.
    h.insert("referrer-policy", HeaderValue::from_static("same-origin"));
    resp
}

/// An error fragment reached a plain browser request (no JavaScript): wrap
/// it in a minimal page with a way back.
async fn error_page(resp: Response) -> Response {
    let status = resp.status();
    let bytes = axum::body::to_bytes(resp.into_body(), 64 * 1024).await.unwrap_or_default();
    let fragment = String::from_utf8_lossy(&bytes);
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" \
         content=\"width=device-width, initial-scale=1\"><title>CalZone</title><link rel=\"stylesheet\" \
         href=\"/static/app.css\"></head><body><main class=\"page auth\"><section class=\"panel\">{fragment}\
         <p>Use your browser's back button, or <a href=\"/\">start over</a>.</p></section></main></body></html>"
    );
    (status, Html(page)).into_response()
}

/// A failed page action: an HTML snippet that HTMX swaps into the page's
/// `#flash` box (HX-Retarget), so a failed action never replaces a form.
#[derive(Debug)]
pub struct WebError {
    pub status: StatusCode,
    pub message: String,
}

impl WebError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self { status, message: message.into() }
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
            DomainError::Invalid(m) => (StatusCode::BAD_REQUEST, capitalise(&m)),
            DomainError::Db(e) => {
                tracing::error!(error = %e, "web: database error");
                (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong on the server.".to_string())
            }
            other => (StatusCode::BAD_REQUEST, capitalise(&other.to_string())),
        };
        WebError::new(status, msg)
    }
}

fn capitalise(s: &str) -> String {
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
        r
    }
}

pub type Page = Result<Response, WebError>;

/// What every signed-in page needs for the header.
pub struct Nav {
    pub label: String,
    pub admin: bool,
    pub dav: bool,
    pub active: &'static str,
}

impl Nav {
    pub fn new(st: &AppState, u: &WebUser, active: &'static str) -> Self {
        Nav { label: u.account.label().to_string(), admin: u.account.is_admin, dav: st.dav, active }
    }

    /// `aria-current` for a section's link.
    pub fn on(&self, section: &str) -> &'static str {
        if self.active == section { "page" } else { "false" }
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

/// A colour for `<input type="color">` (`#rrggbb`), from what clients store
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
    fn colours_for_the_picker() {
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
        assert_eq!(capitalise("a label of 1 to 100 characters"), "A label of 1 to 100 characters.");
        assert_eq!(escape("<a href=\"x\">'"), "&lt;a href=&quot;x&quot;&gt;&#39;");
    }
}
