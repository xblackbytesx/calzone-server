//! HTTP Basic with app passwords, and per-address failure throttling.

use std::time::Instant;

use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine;

use crate::AppState;
use crate::audit::{self, Reason, Service};
use crate::client::ClientAddr;
use crate::domain::app_passwords::{self, Login};
use crate::throttle::WINDOW;

/// Failures allowed per address in one window before it is refused outright.
pub const MAX_FAILURES: u32 = 10;

fn basic(headers: &HeaderMap) -> Option<(String, String)> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, rest) = value.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let decoded = base64::engine::general_purpose::STANDARD.decode(rest.trim()).ok()?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, pass) = text.split_once(':')?;
    Some((user.to_string(), pass.to_string()))
}

fn unauthorized() -> Response {
    (StatusCode::UNAUTHORIZED, [(header::WWW_AUTHENTICATE, "Basic realm=\"CalZone\", charset=\"UTF-8\"")])
        .into_response()
}

/// The login for a request, or the response to send instead: 401 for no or
/// wrong credentials (the same for every kind of failure), 429 for an
/// address over its failure budget.
pub async fn authenticate(st: &AppState, addr: &ClientAddr, headers: &HeaderMap) -> Result<Login, Response> {
    let now = Instant::now();
    let key = addr.key("dav");
    if st.throttle.blocked(&key, MAX_FAILURES, now) {
        audit::auth_failure(Service::Dav, Reason::Throttled, addr.0);
        let retry = [(header::RETRY_AFTER, WINDOW.as_secs().to_string())];
        return Err((StatusCode::TOO_MANY_REQUESTS, retry).into_response());
    }
    // No credentials yet is the normal first request, not a failure.
    let Some((user, pass)) = basic(headers) else { return Err(unauthorized()) };
    match app_passwords::verify(&st.pool, &user, &pass).await {
        Ok(Some(login)) => Ok(login),
        Ok(None) => {
            st.throttle.fail(&key, now);
            audit::auth_failure(Service::Dav, Reason::Wrong, addr.0);
            Err(unauthorized())
        }
        Err(e) => {
            tracing::error!(error = %e, "dav: login check failed");
            Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn basic_header() {
        let mut h = HeaderMap::new();
        assert_eq!(basic(&h), None);
        // "anna:ab:cd" (colons after the first belong to the password)
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Basic YW5uYTphYjpjZA=="));
        assert_eq!(basic(&h), Some(("anna".into(), "ab:cd".into())));
        h.insert(header::AUTHORIZATION, HeaderValue::from_static("Bearer xyz"));
        assert_eq!(basic(&h), None);
    }
}
