//! Browser sessions: an `HttpOnly`, `SameSite=Strict` cookie (`Secure`
//! unless `CALZONE_COOKIE_SECURE=off`) holding 256 random bits that the
//! store keeps only as SHA-256.

use axum::extract::FromRequestParts;
use axum::http::StatusCode;
use axum::http::header::COOKIE;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Redirect, Response};

use super::WebError;
use crate::AppState;
use crate::domain::accounts::{self, Account, SESSION_DAYS};
use calzone_core::quickadd::Lang;

pub const COOKIE_NAME: &str = "calzone_session";

pub fn cookie(value: &str, max_age: i64, secure: bool) -> String {
    format!(
        "{COOKIE_NAME}={value}; Path=/; Max-Age={max_age}; HttpOnly; SameSite=Strict{}",
        if secure { "; Secure" } else { "" }
    )
}

/// A full session's cookie. Persistent: kept 30 days. Otherwise a browser
/// session cookie (no Max-Age): gone when the browser closes, and the
/// server ends the session after 12 hours anyway.
pub fn session_cookie(token: &str, secure: bool, persistent: bool) -> String {
    if persistent {
        cookie(token, SESSION_DAYS * 86_400, secure)
    } else {
        format!("{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict{}", if secure { "; Secure" } else { "" })
    }
}

pub fn pending_cookie(token: &str, secure: bool) -> String {
    cookie(token, accounts::PENDING_MINUTES * 60, secure)
}

fn token(parts: &Parts) -> Option<String> {
    parts
        .headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE_NAME)
        .map(|(_, v)| v.to_string())
        // 32 bytes, base64url without padding.
        .filter(|v| v.len() == 43 && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'))
}

/// A signed-in user. Missing or expired sessions go to `/login`.
#[derive(Debug, Clone)]
pub struct WebUser {
    pub account: Account,
    pub token: String,
    /// Their language: the setting, else the browser's (decision 35).
    pub lang: Lang,
}

impl WebUser {
    pub fn id(&self) -> uuid::Uuid {
        self.account.id
    }
}

/// Where a request without the right session goes: `/login`, or the
/// two-factor setup page for someone who must set it up first.
pub struct ToLogin {
    htmx: bool,
    to: &'static str,
}

impl ToLogin {
    fn login(parts: &Parts) -> Self {
        ToLogin { htmx: parts.headers.contains_key("hx-request"), to: "/login" }
    }
}

impl IntoResponse for ToLogin {
    fn into_response(self) -> Response {
        if self.htmx {
            // A redirect would be followed by the XHR and swapped into a
            // fragment; tell HTMX to load the whole page instead.
            ([("hx-redirect", self.to)], "").into_response()
        } else {
            Redirect::to(self.to).into_response()
        }
    }
}

/// A verified session (passphrase, and the code when two-factor is on).
async fn verified(parts: &Parts, st: &AppState) -> Result<WebUser, ToLogin> {
    let token = token(parts).ok_or_else(|| ToLogin::login(parts))?;
    match accounts::session_user(&st.pool, &token).await {
        Ok(Some(account)) => {
            let lang = super::i18n::lang(account.language.as_deref(), &parts.headers);
            Ok(WebUser { account, token, lang })
        }
        Ok(None) => Err(ToLogin::login(parts)),
        Err(e) => {
            tracing::error!(error = %e, "web: session lookup failed");
            Err(ToLogin::login(parts))
        }
    }
}

impl WebUser {
    /// Whether this person must set up two-factor before using the app.
    pub fn must_enrol(&self, st: &AppState) -> bool {
        !self.account.has_totp && st.two_factor.required_for(self.account.is_admin)
    }
}

/// Every page but the two-factor setup: someone who must use two-factor
/// and has not set it up yet is sent to set it up.
impl FromRequestParts<AppState> for WebUser {
    type Rejection = ToLogin;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let u = verified(parts, st).await?;
        if u.must_enrol(st) {
            return Err(ToLogin { htmx: parts.headers.contains_key("hx-request"), to: "/settings/two-factor" });
        }
        Ok(u)
    }
}

/// A signed-in user for the JSON API: a JSON 401 instead of a redirect,
/// and 403 for someone who must set up two-factor first.
pub struct ApiUser(pub WebUser);

impl FromRequestParts<AppState> for ApiUser {
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let json =
            |status: StatusCode, msg: &str| (status, axum::Json(serde_json::json!({ "error": msg }))).into_response();
        let u = verified(parts, st).await.map_err(|_| json(StatusCode::UNAUTHORIZED, "Signed out. Log in again."))?;
        if u.must_enrol(st) {
            return Err(json(StatusCode::FORBIDDEN, "Set up two-factor sign-in first."));
        }
        Ok(ApiUser(u))
    }
}

/// Signed in, two-factor set up or not: the setup pages and logout.
pub struct SignedIn(pub WebUser);

impl FromRequestParts<AppState> for SignedIn {
    type Rejection = ToLogin;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        verified(parts, st).await.map(SignedIn)
    }
}

/// Passphrase accepted, code still to come: reaches only the code page.
pub struct PendingUser(pub WebUser);

impl FromRequestParts<AppState> for PendingUser {
    type Rejection = ToLogin;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let token = token(parts).ok_or_else(|| ToLogin::login(parts))?;
        match accounts::pending_user(&st.pool, &token).await {
            Ok(Some(account)) => {
                let lang = super::i18n::lang(account.language.as_deref(), &parts.headers);
                Ok(PendingUser(WebUser { account, token, lang }))
            }
            _ => Err(ToLogin::login(parts)),
        }
    }
}

/// Pages that work for both (`/`): no session is simply `None`.
impl axum::extract::OptionalFromRequestParts<AppState> for SignedIn {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Option<Self>, Self::Rejection> {
        Ok(verified(parts, st).await.ok().map(SignedIn))
    }
}

/// The session cookie, whatever state its session is in (logout).
pub struct SessionToken(pub Option<String>);

impl FromRequestParts<AppState> for SessionToken {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &AppState) -> Result<Self, Self::Rejection> {
        Ok(SessionToken(token(parts)))
    }
}

/// A signed-in admin; anyone else gets 403.
pub struct AdminUser(pub WebUser);

pub enum AdminRejection {
    Login(ToLogin),
    Forbidden,
}

impl IntoResponse for AdminRejection {
    fn into_response(self) -> Response {
        match self {
            AdminRejection::Login(l) => l.into_response(),
            AdminRejection::Forbidden => WebError {
                key: Some("admin_only"),
                ..WebError::new(StatusCode::FORBIDDEN, "Only an admin can do that.")
            }
            .into_response(),
        }
    }
}

impl FromRequestParts<AppState> for AdminUser {
    type Rejection = AdminRejection;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let u = WebUser::from_request_parts(parts, st).await.map_err(AdminRejection::Login)?;
        if u.account.is_admin { Ok(AdminUser(u)) } else { Err(AdminRejection::Forbidden) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_flags() {
        assert_eq!(
            cookie("abc", 60, true),
            "calzone_session=abc; Path=/; Max-Age=60; HttpOnly; SameSite=Strict; Secure"
        );
        assert!(!cookie("abc", 60, false).contains("Secure"));
        assert!(session_cookie("abc", true, true).contains("Max-Age=2592000"));
        let short = session_cookie("abc", true, false);
        assert!(!short.contains("Max-Age") && short.contains("HttpOnly; SameSite=Strict; Secure"));
    }
}
