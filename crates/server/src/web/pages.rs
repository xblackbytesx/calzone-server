//! Login, logout and personal settings.

use std::time::Instant;

use askama::Template;
use axum::Form;
use axum::extract::{RawQuery, State};
use axum::http::header::SET_COOKIE;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;

use super::session::{self, SessionToken, SignedIn, WebUser};
use super::{Nav, Page, WebError, html, i18n};
use crate::AppState;
use crate::audit::{self, Reason, Service};
use crate::client::ClientAddr;
use crate::domain::accounts::{self, MIN_PASSPHRASE};
use crate::domain::two_factor;

/// Web login failures allowed per address and per account in a window.
pub(super) const PER_ADDRESS: u32 = 10;
pub(super) const PER_ACCOUNT: u32 = 5;

pub async fn home(State(st): State<AppState>, user: Option<SignedIn>) -> Redirect {
    Redirect::to(match user {
        Some(SignedIn(u)) if u.must_enrol(&st) => "/settings/two-factor",
        Some(_) => "/calendar",
        None => "/login",
    })
}

#[derive(Template)]
#[template(path = "login.html")]
struct LoginPage {
    name: String,
    error: Option<&'static str>,
    t: &'static i18n::T,
    lang: &'static str,
}

/// Before login the browser's language decides (decision 35).
fn browser_lang(headers: &HeaderMap) -> (&'static i18n::T, &'static str) {
    let l = i18n::lang(None, headers);
    (i18n::t(l), i18n::code(l))
}

pub async fn login_page(headers: HeaderMap) -> Page {
    let (t, lang) = browser_lang(&headers);
    html(LoginPage { name: String::new(), error: None, t, lang })
}

#[derive(Deserialize)]
pub struct LoginForm {
    name: String,
    passphrase: String,
    /// "Keep me signed in on this device": present when ticked.
    #[serde(default)]
    remember: Option<String>,
}

fn account_key(name: &str) -> String {
    format!("web-user:{}", name.trim().to_ascii_lowercase())
}

pub async fn login(State(st): State<AppState>, addr: ClientAddr, headers: HeaderMap, Form(f): Form<LoginForm>) -> Page {
    let (t, lang) = browser_lang(&headers);
    let again = |error, status| -> Page {
        let mut r = html(LoginPage { name: f.name.clone(), error: Some(error), t, lang })?;
        *r.status_mut() = status;
        Ok(r)
    };
    let now = Instant::now();
    let (addr_key, acct_key) = (addr.key("web"), account_key(&f.name));
    // Checked before Argon2 runs, so a blocked guesser costs no CPU.
    if st.throttle.blocked(&addr_key, PER_ADDRESS, now) || st.throttle.blocked(&acct_key, PER_ACCOUNT, now) {
        audit::auth_failure(Service::Web, Reason::Throttled, addr.0);
        return again(t.too_many_logins, StatusCode::TOO_MANY_REQUESTS);
    }
    match accounts::check_login(&st.pool, &f.name, &f.passphrase).await? {
        Some(user) => {
            st.throttle.clear(&acct_key);
            // With two-factor on, the passphrase only opens the code page.
            let persistent = f.remember.is_some();
            if accounts::get(&st.pool, user).await?.has_totp {
                let token = accounts::create_pending_session(&st.pool, user, persistent).await?;
                return Ok((
                    [(SET_COOKIE, session::pending_cookie(&token, st.cookie_secure))],
                    Redirect::to("/login/code"),
                )
                    .into_response());
            }
            let token = accounts::create_session(&st.pool, user, persistent).await?;
            Ok((
                [(SET_COOKIE, session::session_cookie(&token, st.cookie_secure, persistent))],
                Redirect::to("/calendar"),
            )
                .into_response())
        }
        None => {
            st.throttle.fail(&addr_key, now);
            st.throttle.fail(&acct_key, now);
            audit::auth_failure(Service::Web, Reason::Wrong, addr.0);
            // The same answer for an unknown name, a disabled account, no
            // passphrase yet and a wrong passphrase.
            again(t.wrong_login, StatusCode::UNAUTHORIZED)
        }
    }
}

/// Ends whatever session the cookie holds (also a pending or a
/// not-yet-set-up one).
pub async fn logout(State(st): State<AppState>, SessionToken(token): SessionToken) -> Page {
    if let Some(t) = token {
        accounts::end_session(&st.pool, &t).await?;
    }
    Ok(([(SET_COOKIE, session::cookie("", 0, st.cookie_secure))], Redirect::to("/login")).into_response())
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsPage {
    nav: Nav,
    name: String,
    display_name: String,
    min: usize,
    saved: Option<&'static str>,
    has_totp: bool,
    totp_required: bool,
    codes_left: i64,
}

pub async fn settings(State(st): State<AppState>, user: WebUser, RawQuery(q): RawQuery) -> Page {
    let used = q.as_deref() == Some("recovery=used");
    settings_page(
        &st,
        &user,
        used.then_some(
            "You signed in with a recovery code; it no longer works. Make new codes below when few are left.",
        ),
    )
    .await
}

pub async fn settings_page(st: &AppState, user: &WebUser, saved: Option<&'static str>) -> Page {
    let codes_left =
        if user.account.has_totp { two_factor::unused_recovery_codes(&st.pool, user.id()).await? } else { 0 };
    html(SettingsPage {
        nav: Nav::new(st, user, "settings"),
        name: user.account.name.clone(),
        display_name: user.account.display_name.clone(),
        min: MIN_PASSPHRASE,
        saved,
        has_totp: user.account.has_totp,
        totp_required: st.two_factor.required_for(user.account.is_admin),
        codes_left,
    })
}

#[derive(Deserialize)]
pub struct ProfileForm {
    display_name: String,
}

pub async fn save_profile(State(st): State<AppState>, user: WebUser, Form(f): Form<ProfileForm>) -> Page {
    accounts::set_display_name(&st.pool, user.id(), &f.display_name).await?;
    Ok(Redirect::to("/settings").into_response())
}

#[derive(Deserialize)]
pub struct PassphraseForm {
    current: String,
    new: String,
    repeat: String,
}

/// Changing the passphrase ends every session, this one included, and
/// starts a fresh one here, so other browsers are signed out.
pub async fn change_passphrase(State(st): State<AppState>, user: WebUser, Form(f): Form<PassphraseForm>) -> Page {
    if f.new != f.repeat {
        return Err(WebError::bad("The two new passphrases differ."));
    }
    if !accounts::passphrase_matches(&st.pool, user.id(), &f.current).await? {
        return Err(WebError::new(StatusCode::FORBIDDEN, "Your current passphrase is not right."));
    }
    accounts::set_passphrase(&st.pool, user.id(), &f.new).await?;
    // The new session keeps this one's "keep me signed in" choice.
    let persistent = accounts::session_persistent(&st.pool, &user.token).await?;
    let token = accounts::create_session(&st.pool, user.id(), persistent).await?;
    let mut r = settings_page(&st, &user, Some("Passphrase changed. Other browsers are signed out.")).await?;
    if let Ok(v) = session::session_cookie(&token, st.cookie_secure, persistent).parse() {
        r.headers_mut().insert(SET_COOKIE, v);
    }
    Ok(r)
}
