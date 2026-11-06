//! The client's address, for login throttling.

use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::HeaderMap;
use axum::http::request::Parts;

use crate::AppState;

/// Behind a proxy (declared with `CALZONE_BEHIND_PROXY`) it is the right-most
/// `X-Forwarded-For` entry, the one the proxy appended; otherwise the socket
/// peer. None in tests, which have neither.
pub struct ClientAddr(pub Option<IpAddr>);

impl ClientAddr {
    /// A throttle key for this address under a prefix.
    pub fn key(&self, prefix: &str) -> String {
        match self.0 {
            Some(ip) => format!("{prefix}:{ip}"),
            None => format!("{prefix}:unknown"),
        }
    }
}

impl FromRequestParts<AppState> for ClientAddr {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        if !st.behind_proxy {
            return Ok(ClientAddr(peer));
        }
        Ok(ClientAddr(forwarded_for(&parts.headers).or(peer)))
    }
}

fn forwarded_for(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .next_back()
        .and_then(|s| s.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    #[test]
    fn forwarded_for_takes_the_proxys_entry() {
        let mut h = HeaderMap::new();
        assert_eq!(forwarded_for(&h), None);
        h.append("x-forwarded-for", HeaderValue::from_static("6.6.6.6, 203.0.113.7"));
        assert_eq!(forwarded_for(&h), "203.0.113.7".parse().ok());
        h.append("x-forwarded-for", HeaderValue::from_static("not-an-ip"));
        assert_eq!(forwarded_for(&h), None, "garbage counts as unknown, not as the forged entry");
    }
}
