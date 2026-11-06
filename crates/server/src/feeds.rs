//! Subscribed feeds (decision 24): fetching an outside `.ics` safely, and
//! the refresher that keeps subscribed calendars current.
//!
//! The fetch is guarded against server-side request forgery:
//! - the host is resolved once and every address is checked (no loopback,
//!   private, link-local, shared, multicast or unspecified address unless
//!   `CALZONE_FEEDS_ALLOW_PRIVATE` is on);
//! - the connection is pinned to the checked address, so a second lookup
//!   cannot swap in another;
//! - redirects are followed by hand (at most 3), each checked again;
//! - 10 MiB and 30 seconds at most; `webcal://` means `https://`.
//!
//! A refresh replaces the calendar only with a copy that decoded, split
//! and validated completely and is not empty; anything else is recorded as
//! a failure and the last good copy stays.

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use calzone_core::import::{decode, split_calendar};
use reqwest::header::{ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED, LOCATION};
use reqwest::{StatusCode, Url};
use sqlx::PgPool;

use crate::domain::subscriptions::{self, Replaced, Subscription};

pub const MAX_FEED: usize = 10 * 1024 * 1024;
const MAX_REDIRECTS: usize = 3;
const TIMEOUT: Duration = Duration::from_secs(30);

/// Whether an address may be fetched without `CALZONE_FEEDS_ALLOW_PRIVATE`.
pub fn public_address(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => {
            let o = v.octets();
            !(v.is_loopback()
                || v.is_private()
                || v.is_link_local()
                || v.is_multicast()
                || v.is_broadcast()
                || v.is_unspecified()
                || o[0] == 0
                || (o[0] == 100 && (64..=127).contains(&o[1])) // shared / CGNAT
                || (o[0] == 192 && o[1] == 0 && o[2] == 0) // IETF protocol assignments
                || o[0] >= 240)
        }
        IpAddr::V6(v) => {
            if let Some(v4) = v.to_ipv4_mapped() {
                return public_address(IpAddr::V4(v4));
            }
            let s = v.segments();
            !(v.is_loopback()
                || v.is_unspecified()
                || v.is_multicast()
                || (s[0] & 0xfe00) == 0xfc00 // unique local
                || (s[0] & 0xffc0) == 0xfe80 // link-local
                || (s[0] == 0x64 && s[1] == 0xff9b)) // NAT64 to anything
        }
    }
}

/// A subscription address as people paste it: `webcal://` and `webcals://`
/// mean https; only http and https, no user name or password in it.
pub fn check_url(raw: &str) -> Result<Url, String> {
    let raw = raw.trim();
    let raw = if let Some(r) = raw.strip_prefix("webcals://").or_else(|| raw.strip_prefix("webcal://")) {
        format!("https://{r}")
    } else {
        raw.to_string()
    };
    let u = Url::parse(&raw).map_err(|_| "That is not a web address.".to_string())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err("Use an https:// (or webcal://) address.".into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err("The address must not contain a user name or password.".into());
    }
    Ok(u)
}

pub enum Fetched {
    NotModified,
    Body { bytes: Vec<u8>, etag: Option<String>, last_modified: Option<String> },
}

/// Fetch a feed under the guard. Errors are plain sentences without content.
pub async fn fetch(
    url: &Url,
    allow_private: bool,
    etag: Option<&str>,
    last_modified: Option<&str>,
) -> Result<Fetched, String> {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let mut url = url.clone();
    for _ in 0..=MAX_REDIRECTS {
        let host = url.host_str().ok_or("no host")?.to_string();
        let port = url.port_or_known_default().ok_or("no port")?;
        let bare = host.trim_start_matches('[').trim_end_matches(']').to_string();
        let addrs: Vec<SocketAddr> = tokio::net::lookup_host((bare.as_str(), port))
            .await
            .map_err(|_| "The address's host name does not resolve.".to_string())?
            .collect();
        if addrs.is_empty() {
            return Err("The address's host name does not resolve.".into());
        }
        if !allow_private && addrs.iter().any(|a| !public_address(a.ip())) {
            return Err("That address points into a private network, which this server does not fetch from.".into());
        }
        let pinned = addrs[0];
        let client = reqwest::Client::builder()
            .resolve(&bare, pinned)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(TIMEOUT)
            .user_agent(concat!("CalZone/", env!("CARGO_PKG_VERSION"), " (calendar subscription)"))
            .build()
            .map_err(|_| "Could not prepare the request.".to_string())?;
        let mut req = client.get(url.clone()).header("accept", "text/calendar, */*;q=0.5");
        if let Some(e) = etag {
            req = req.header(IF_NONE_MATCH, e);
        }
        if let Some(m) = last_modified {
            req = req.header(IF_MODIFIED_SINCE, m);
        }
        let mut resp = req.send().await.map_err(|e| {
            if e.is_timeout() {
                "The address did not answer in time.".to_string()
            } else {
                "Could not connect to the address.".to_string()
            }
        })?;
        let status = resp.status();
        if status.is_redirection() && status != StatusCode::NOT_MODIFIED {
            let next = resp.headers().get(LOCATION).and_then(|l| l.to_str().ok()).and_then(|l| url.join(l).ok());
            match next {
                Some(n) if matches!(n.scheme(), "http" | "https") => {
                    url = n;
                    continue;
                }
                _ => return Err("The address redirected somewhere unusable.".into()),
            }
        }
        if status == StatusCode::NOT_MODIFIED {
            return Ok(Fetched::NotModified);
        }
        if !status.is_success() {
            return Err(format!("The address answered with HTTP {}.", status.as_u16()));
        }
        let header =
            |h| resp.headers().get(h).and_then(|v: &reqwest::header::HeaderValue| v.to_str().ok()).map(str::to_string);
        let (etag, last_modified) = (header(ETAG), header(LAST_MODIFIED));
        if resp.content_length().is_some_and(|l| l as usize > MAX_FEED) {
            return Err("The feed is larger than 10 MiB.".into());
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = resp.chunk().await.map_err(|_| "The download broke off.".to_string())? {
            bytes.extend_from_slice(&chunk);
            if bytes.len() > MAX_FEED {
                return Err("The feed is larger than 10 MiB.".into());
            }
        }
        return Ok(Fetched::Body { bytes, etag, last_modified });
    }
    Err("The address redirected too many times.".into())
}

/// The outcome of one refresh, for the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Unchanged,
    Updated(Replaced),
    Failed(String),
}

/// Bytes to objects, or why not (the whole copy or nothing).
pub fn read_feed(bytes: &[u8]) -> Result<(Option<String>, Vec<String>), String> {
    let (text, _) = decode(bytes).map_err(|_| "The feed is not UTF-8 text.".to_string())?;
    let file = split_calendar(&text).map_err(|_| "The address does not serve an iCalendar (.ics) feed.".to_string())?;
    Ok((file.name, file.objects))
}

/// Refresh one subscription now and record the result.
pub async fn refresh(pool: &PgPool, sub: &Subscription, allow_private: bool) -> Outcome {
    let outcome = match check_url(&sub.url) {
        Err(e) => Outcome::Failed(e),
        Ok(url) => match fetch(&url, allow_private, sub.etag.as_deref(), sub.last_modified.as_deref()).await {
            Err(e) => Outcome::Failed(e),
            Ok(Fetched::NotModified) => {
                let _ = subscriptions::record_success(pool, sub.collection_id, None, None).await;
                return Outcome::Unchanged;
            }
            Ok(Fetched::Body { bytes, etag, last_modified }) => match read_feed(&bytes) {
                Err(e) => Outcome::Failed(e),
                Ok((_, objects)) => match subscriptions::replace(pool, sub.collection_id, &objects).await {
                    Ok(r) => {
                        let _ = subscriptions::record_success(
                            pool,
                            sub.collection_id,
                            etag.as_deref(),
                            last_modified.as_deref(),
                        )
                        .await;
                        return Outcome::Updated(r);
                    }
                    Err(crate::domain::DomainError::Invalid(why)) => Outcome::Failed(capitalised(&why)),
                    Err(e) => {
                        tracing::warn!(collection = %sub.collection_id, error = %e, "feeds: replace failed");
                        Outcome::Failed("Something went wrong on the server; the last good copy is kept.".into())
                    }
                },
            },
        },
    };
    if let Outcome::Failed(why) = &outcome {
        let _ = subscriptions::record_failure(pool, sub.collection_id, why).await;
    }
    outcome
}

fn capitalised(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str() + if s.ends_with('.') { "" } else { "." },
        None => String::new(),
    }
}

/// Every minute: refresh what is due, a few at a time.
pub fn spawn_refresher(pool: PgPool, allow_private: bool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        loop {
            tick.tick().await;
            match subscriptions::due(&pool, 10).await {
                Ok(due) => {
                    for sub in due {
                        if let Outcome::Failed(why) = refresh(&pool, &sub, allow_private).await {
                            tracing::info!(collection = %sub.collection_id, reason = %why, "feeds: refresh failed; last good copy kept");
                        }
                    }
                }
                Err(e) => tracing::warn!(error = %e, "feeds: could not list due subscriptions"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn private_addresses_are_refused() {
        for bad in [
            "127.0.0.1",
            "10.1.2.3",
            "172.16.0.9",
            "192.168.1.10",
            "169.254.169.254",
            "100.64.0.1",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
            "64:ff9b::a00:1",
        ] {
            assert!(!public_address(bad.parse().unwrap()), "{bad}");
        }
        for good in ["8.8.8.8", "142.250.180.14", "2a00:1450:400e:80d::200e", "::ffff:8.8.8.8"] {
            assert!(public_address(good.parse().unwrap()), "{good}");
        }
    }

    #[test]
    fn addresses_as_people_paste_them() {
        assert_eq!(
            check_url("webcal://calendar.google.com/x/basic.ics").unwrap().as_str(),
            "https://calendar.google.com/x/basic.ics"
        );
        assert_eq!(check_url(" webcals://a.example/f.ics ").unwrap().scheme(), "https");
        assert!(check_url("ftp://a.example/f.ics").is_err());
        assert!(check_url("https://me:pw@a.example/f.ics").is_err());
        assert!(check_url("not a url").is_err());
    }

    #[tokio::test]
    async fn the_guard_refuses_before_connecting() {
        let u = Url::parse("http://127.0.0.1:9/feed.ics").unwrap();
        match fetch(&u, false, None, None).await {
            Err(e) => assert!(e.contains("private network"), "{e}"),
            Ok(_) => panic!("fetched a loopback address"),
        }
        let u = Url::parse("http://localhost:9/feed.ics").unwrap();
        assert!(
            fetch(&u, false, None, None).await.is_err_and(|e| e.contains("private network")),
            "names resolving to loopback too"
        );
    }

    #[test]
    fn feeds_read_whole_or_not_at_all() {
        assert!(read_feed(b"<html>nope</html>").is_err());
        assert!(read_feed(b"\xff\xfe").is_err());
        let ok = b"BEGIN:VCALENDAR\r\nX-WR-CALNAME:Holidays\r\nBEGIN:VEVENT\r\nUID:a\r\nDTSTART;VALUE=DATE:20261225\r\nSUMMARY:Christmas\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let (name, objs) = read_feed(ok).unwrap();
        assert_eq!((name.as_deref(), objs.len()), (Some("Holidays"), 1));
    }
}
