//! Two-factor sign-in pages (decision 32): the code step after the
//! passphrase, setup with a QR code, recovery codes, and turning it off.
//! Wrong codes count against the same budgets as wrong passphrases.

use std::time::Instant;

use askama::Template;
use axum::Form;
use axum::extract::State;
use axum::http::StatusCode;
use axum::http::header::SET_COOKIE;
use axum::response::{IntoResponse, Redirect};
use serde::Deserialize;

use super::pages::{PER_ACCOUNT, PER_ADDRESS};
use super::session::{self, PendingUser, SignedIn, WebUser};
use super::{Nav, Page, WebError, html};
use crate::AppState;
use crate::audit::{self, Reason, Service};
use crate::client::ClientAddr;
use crate::domain::accounts;
use crate::domain::two_factor::{self, Accepted, SecretKey};

fn key(st: &AppState) -> Result<&SecretKey, WebError> {
    st.secret_key.as_ref().ok_or_else(|| {
        WebError::new(StatusCode::SERVICE_UNAVAILABLE, "Two-factor sign-in needs CALZONE_SECRET_KEY on the server.")
    })
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

#[derive(Template)]
#[template(path = "login_code.html")]
struct CodePage {
    error: Option<&'static str>,
}

pub async fn code_page(_: PendingUser) -> Page {
    html(CodePage { error: None })
}

#[derive(Deserialize)]
pub struct CodeForm {
    code: String,
}

fn budget_key(user: uuid::Uuid) -> String {
    format!("web-2fa:{user}")
}

/// The second step of the login: a code from the app or a recovery code.
/// Success replaces the pending session with a fresh, full one.
pub async fn code(
    State(st): State<AppState>,
    PendingUser(user): PendingUser,
    addr: ClientAddr,
    Form(f): Form<CodeForm>,
) -> Page {
    let again = |error, status| -> Page {
        let mut r = html(CodePage { error: Some(error) })?;
        *r.status_mut() = status;
        Ok(r)
    };
    let t = Instant::now();
    let (addr_key, acct_key) = (addr.key("web"), budget_key(user.id()));
    if st.throttle.blocked(&addr_key, PER_ADDRESS, t) || st.throttle.blocked(&acct_key, PER_ACCOUNT, t) {
        // Too many tries: this pending session is spent; start over later.
        audit::auth_failure(Service::Code, Reason::Throttled, addr.0);
        accounts::end_session(&st.pool, &user.token).await?;
        return again("Too many wrong codes. Wait 15 minutes and log in again.", StatusCode::TOO_MANY_REQUESTS);
    }
    match two_factor::check(&st.pool, key(&st)?, user.id(), &f.code, now()).await? {
        Some(how) => {
            st.throttle.clear(&acct_key);
            let persistent = accounts::session_persistent(&st.pool, &user.token).await?;
            accounts::end_session(&st.pool, &user.token).await?;
            let token = accounts::create_session(&st.pool, user.id(), persistent).await?;
            let to = if how == Accepted::RecoveryCode { "/settings?recovery=used" } else { "/collections" };
            Ok(([(SET_COOKIE, session::session_cookie(&token, st.cookie_secure, persistent))], Redirect::to(to))
                .into_response())
        }
        None => {
            st.throttle.fail(&addr_key, t);
            st.throttle.fail(&acct_key, t);
            audit::auth_failure(Service::Code, Reason::Wrong, addr.0);
            again("That code is not right, or was already used. Try the next one.", StatusCode::UNAUTHORIZED)
        }
    }
}

#[derive(Template)]
#[template(path = "two_factor_setup.html")]
struct SetupPage {
    nav: Nav,
    required: bool,
    qr: String,
    secret: String,
}

/// Setup: a QR code for the authenticator app, and the secret typed out
/// for apps that cannot scan.
pub async fn setup(State(st): State<AppState>, SignedIn(user): SignedIn) -> Page {
    if user.account.has_totp {
        return Ok(Redirect::to("/settings").into_response());
    }
    let secret = two_factor::begin_setup(&st.pool, key(&st)?, user.id()).await?;
    let b32 = two_factor::base32(&secret);
    let grouped: Vec<String> = b32.as_bytes().chunks(4).map(|c| String::from_utf8_lossy(c).into_owned()).collect();
    html(SetupPage {
        nav: Nav::new(&st, &user, "settings"),
        required: user.must_enrol(&st),
        qr: two_factor::qr_svg(&two_factor::otpauth_uri(&user.account.name, &secret)),
        secret: grouped.join(" "),
    })
}

#[derive(Template)]
#[template(path = "recovery_codes.html")]
struct CodesPage {
    nav: Nav,
    codes: Vec<String>,
    first: bool,
}

pub async fn confirm(State(st): State<AppState>, SignedIn(user): SignedIn, Form(f): Form<CodeForm>) -> Page {
    if user.account.has_totp {
        return Ok(Redirect::to("/settings").into_response());
    }
    match two_factor::confirm_setup(&st.pool, key(&st)?, user.id(), &f.code, now(), &user.token).await? {
        Some(codes) => {
            let user = WebUser { account: accounts::get(&st.pool, user.id()).await?, ..user };
            html(CodesPage { nav: Nav::new(&st, &user, "settings"), codes, first: true })
        }
        None => Err(WebError::bad("That code is not right. Check the time on your phone, and use the newest code.")),
    }
}

/// New recovery codes, with a current code from the app to prove it is you.
pub async fn new_codes(State(st): State<AppState>, user: WebUser, Form(f): Form<CodeForm>) -> Page {
    if two_factor::check(&st.pool, key(&st)?, user.id(), &f.code, now()).await? != Some(Accepted::App) {
        return Err(WebError::new(StatusCode::FORBIDDEN, "That code from your app is not right."));
    }
    let codes = two_factor::new_recovery_codes(&st.pool, user.id()).await?;
    html(CodesPage { nav: Nav::new(&st, &user, "settings"), codes, first: false })
}

#[derive(Deserialize)]
pub struct OffForm {
    passphrase: String,
    code: String,
}

/// Turning it off needs the passphrase and a code, and is not possible for
/// someone who must use it. Every session ends; the person logs in again.
pub async fn turn_off(State(st): State<AppState>, user: WebUser, Form(f): Form<OffForm>) -> Page {
    if st.two_factor.required_for(user.account.is_admin) {
        return Err(WebError::bad("Two-factor sign-in is required for your account on this server."));
    }
    if !accounts::passphrase_matches(&st.pool, user.id(), &f.passphrase).await?
        || two_factor::check(&st.pool, key(&st)?, user.id(), &f.code, now()).await?.is_none()
    {
        return Err(WebError::new(StatusCode::FORBIDDEN, "The passphrase or the code is not right."));
    }
    two_factor::disable(&st.pool, user.id()).await?;
    Ok(([(SET_COOKIE, session::cookie("", 0, st.cookie_secure))], Redirect::to("/login")).into_response())
}
