//! UnifiedPush to the phones (decision 16). Every door's writes reach the
//! database through the domain, and the database announces them on one
//! channel (`calzone_changes`, migration 0010) when they commit. One task
//! listens, gathers what changed for a moment, and POSTs each phone that
//! can see it a small message encrypted to that phone's keys (`webpush`):
//! which collections changed and their state now, nothing about what. The
//! phone then syncs. Push is a hint only: a lost message costs freshness,
//! never data, because the phone also syncs on Android's own schedule.
//!
//! Rules from wordwarz-server: endpoints only on the declared hosts
//! (`CALZONE_PUSH_HOSTS`), https only, no redirects, a `404`/`410` drops the
//! endpoint, and the URL (a capability) never reaches the log.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use calzone_core::sync::{Changed, Notice, State};
use sqlx::PgPool;
use sqlx::postgres::PgListener;
use uuid::Uuid;

use crate::domain::access::level_sql;
use crate::domain::devices;
use crate::webpush::{self, DeviceKeys};

pub const CHANNEL: &str = "calzone_changes";
/// How long to gather notices before sending: an import or a series edit
/// is one push, not hundreds.
pub const GATHER: Duration = Duration::from_secs(2);
/// More collections than this in one message: send `all` instead, so the
/// message stays far below the 4 KB push servers accept.
const MAX_LISTED: usize = 40;

/// Where pushes may go. Empty hosts: push is off.
#[derive(Clone)]
pub struct Settings {
    /// `CALZONE_PUSH_HOSTS`: host names (with a port if not 443) of the
    /// UnifiedPush servers phones may register endpoints on.
    pub hosts: Vec<String>,
    /// Tests only: plain-http endpoints (a local stand-in push server).
    pub allow_http: bool,
    pub http: reqwest::Client,
}

impl Settings {
    pub fn new(hosts: Vec<String>) -> Self {
        Settings { hosts, allow_http: false, http: client() }
    }

    pub fn off() -> Self {
        Settings::new(Vec::new())
    }

    pub fn on(&self) -> bool {
        !self.hosts.is_empty()
    }
}

fn client() -> reqwest::Client {
    // reqwest is built without a default crypto provider (aws-lc needs cmake
    // everywhere); ring is installed once, before any client exists.
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(15))
        // The endpoint was checked against the declared hosts; a redirect
        // could lead anywhere.
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("CalZone/", env!("CARGO_PKG_VERSION"), " (push)"))
        .build()
        .expect("reqwest client")
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum EndpointError {
    #[error("push is not set up on this server (CALZONE_PUSH_HOSTS)")]
    Off,
    #[error("a push endpoint is an https:// address")]
    NotHttps,
    #[error("that is not a push endpoint")]
    Malformed,
    #[error("this server only sends pushes to: {0}")]
    HostNotAllowed(String),
    #[error(transparent)]
    Keys(#[from] webpush::KeyError),
}

/// Check an endpoint against the declared hosts: https (http only for
/// tests), a listed host, no credentials, no spaces. Returns it trimmed.
pub fn check_endpoint(s: &Settings, url: &str) -> Result<String, EndpointError> {
    if !s.on() {
        return Err(EndpointError::Off);
    }
    let url = url.trim();
    let rest = url
        .strip_prefix("https://")
        .or_else(|| if s.allow_http { url.strip_prefix("http://") } else { None })
        .ok_or(EndpointError::NotHttps)?;
    let host = rest.split(['/', '?', '#']).next().unwrap_or("").to_ascii_lowercase();
    if url.len() > 1000
        || host.is_empty()
        || host.contains('@')
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(EndpointError::Malformed);
    }
    let bare = host.strip_suffix(":443").unwrap_or(&host);
    if !s.hosts.iter().any(|h| h == bare) {
        return Err(EndpointError::HostNotAllowed(s.hosts.join(", ")));
    }
    Ok(url.to_string())
}

/// `CALZONE_PUSH_HOSTS`: comma-separated host names, lowercased. URLs are
/// refused so a misread setting fails at start, not at every push.
pub fn parse_hosts(v: Option<&str>) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    for h in v.unwrap_or("").split(',').map(str::trim).filter(|h| !h.is_empty()) {
        if h.contains("://") || h.contains('/') || h.contains('@') {
            anyhow::bail!("CALZONE_PUSH_HOSTS takes host names like ntfy.example.com, not URLs: {h}");
        }
        out.push(h.to_ascii_lowercase());
    }
    Ok(out)
}

/// What the notices since the last send named.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Pending {
    /// Collections whose content, name, color or sharing changed.
    pub collections: HashSet<Uuid>,
    /// One person's own view of a collection changed (collection, person).
    pub personal: HashSet<(Uuid, Uuid)>,
}

impl Pending {
    /// Add one notice payload; anything else is ignored.
    pub fn add(&mut self, payload: &str) {
        match payload.split_once('/') {
            None => {
                if let Ok(c) = payload.parse() {
                    self.collections.insert(c);
                }
            }
            Some((c, p)) => {
                if let (Ok(c), Ok(p)) = (c.parse(), p.parse()) {
                    self.personal.insert((c, p));
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.collections.is_empty() && self.personal.is_empty()
    }
}

/// Start listening (connected and subscribed before this returns, so no
/// write after it is missed), then gather and send in the background until
/// the process ends. A lost connection is retried.
pub async fn start(pool: PgPool, settings: Arc<Settings>) -> sqlx::Result<tokio::task::JoinHandle<()>> {
    let mut listener = PgListener::connect_with(&pool).await?;
    listener.listen(CHANNEL).await?;
    Ok(tokio::spawn(async move {
        loop {
            if let Err(e) = run(&mut listener, &pool, &settings).await {
                tracing::warn!(error = %e, "push: change notices interrupted; listening again");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }))
}

async fn run(listener: &mut PgListener, pool: &PgPool, settings: &Settings) -> sqlx::Result<()> {
    loop {
        let first = listener.recv().await?;
        let mut pending = Pending::default();
        pending.add(first.payload());
        let until = tokio::time::Instant::now() + GATHER;
        while let Ok(next) = tokio::time::timeout_at(until, listener.recv()).await {
            pending.add(next?.payload());
        }
        if let Err(e) = send(pool, settings, &pending).await {
            tracing::warn!(error = %e, "push: could not look up the phones to wake");
        }
    }
}

/// One phone to wake, and what to tell it.
struct Target {
    url: String,
    key: String,
    auth: String,
    changed: Vec<Changed>,
}

/// Wake every phone whose person can see what changed. Each POST runs on
/// its own, so one slow push server holds up nobody else.
pub async fn send(pool: &PgPool, settings: &Settings, pending: &Pending) -> sqlx::Result<()> {
    if pending.is_empty() || !settings.on() {
        return Ok(());
    }
    let collections: Vec<Uuid> = pending.collections.iter().copied().collect();
    let (pc, pp): (Vec<Uuid>, Vec<Uuid>) = pending.personal.iter().copied().unzip();
    let sql = format!(
        "SELECT d.id, d.push_url, d.push_key, d.push_auth, c.id, c.sequence
         FROM devices d JOIN principals pr ON pr.id = d.principal_id
         JOIN collections c ON c.id = ANY($1)
              OR (c.id, d.principal_id) IN (SELECT * FROM unnest($2::uuid[], $3::uuid[]))
         WHERE d.push_url IS NOT NULL AND NOT pr.disabled AND ({}) IS NOT NULL",
        level_sql("d.principal_id")
    );
    type Row = (Uuid, String, String, String, Uuid, i64);
    let rows: Vec<Row> = sqlx::query_as(&sql).bind(&collections).bind(&pc).bind(&pp).fetch_all(pool).await?;
    let mut targets: BTreeMap<Uuid, Target> = BTreeMap::new();
    for (device, url, key, auth, collection, state) in rows {
        targets
            .entry(device)
            .or_insert_with(|| Target { url, key, auth, changed: Vec::new() })
            .changed
            .push(Changed { id: collection.to_string(), state: State(state) });
    }
    for (device, t) in targets {
        // Re-checked at send time: the declared hosts may have changed.
        if check_endpoint(settings, &t.url).is_err() {
            continue;
        }
        let Ok(keys) = DeviceKeys::parse(&t.key, &t.auth) else { continue };
        let mut changed = t.changed;
        changed.sort_by(|a, b| a.id.cmp(&b.id));
        let notice = if changed.len() > MAX_LISTED {
            Notice { collections: Vec::new(), all: true }
        } else {
            Notice { collections: changed, all: false }
        };
        let body = webpush::encrypt(&keys, &serde_json::to_vec(&notice).expect("serializable"));
        let http = settings.http.clone();
        let pool = pool.clone();
        let url = t.url;
        tokio::spawn(async move {
            let sent = http
                .post(&url)
                .header("ttl", "86400")
                .header("urgency", "normal")
                .header("content-encoding", "aes128gcm")
                .header("content-type", "application/octet-stream")
                .body(body)
                .send()
                .await;
            match sent {
                Ok(r) if r.status().is_success() => {}
                Ok(r) if matches!(r.status().as_u16(), 404 | 410) => {
                    if let Err(e) = devices::clear_push(&pool, device, Some(&url)).await {
                        tracing::warn!(error = %e, "push: could not drop a gone endpoint");
                    } else {
                        tracing::info!(%device, "push: endpoint gone, dropped");
                    }
                }
                Ok(r) => tracing::warn!(status = %r.status(), %device, "push: refused"),
                Err(e) => tracing::warn!(error = %e.without_url(), %device, "push: failed"),
            }
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> Settings {
        Settings::new(vec!["ntfy.example.com".into(), "push.example.org:8443".into()])
    }

    #[test]
    fn endpoints_only_on_declared_hosts() {
        let s = on();
        assert_eq!(
            check_endpoint(&s, " https://ntfy.example.com/upAbc?up=1 ").unwrap(),
            "https://ntfy.example.com/upAbc?up=1"
        );
        assert!(check_endpoint(&s, "https://NTFY.example.com:443/x").is_ok());
        assert!(check_endpoint(&s, "https://push.example.org:8443/x").is_ok());
        assert_eq!(check_endpoint(&s, "http://ntfy.example.com/x"), Err(EndpointError::NotHttps));
        assert!(matches!(check_endpoint(&s, "https://evil.example.com/x"), Err(EndpointError::HostNotAllowed(_))));
        assert!(matches!(check_endpoint(&s, "https://push.example.org/x"), Err(EndpointError::HostNotAllowed(_))));
        assert_eq!(check_endpoint(&s, "https://u:p@ntfy.example.com/x"), Err(EndpointError::Malformed));
        assert_eq!(check_endpoint(&s, "https://ntfy.example.com/a b"), Err(EndpointError::Malformed));
        assert_eq!(check_endpoint(&Settings::off(), "https://ntfy.example.com/x"), Err(EndpointError::Off));
    }

    #[test]
    fn hosts_are_bare_names() {
        assert_eq!(parse_hosts(None).unwrap(), Vec::<String>::new());
        assert_eq!(
            parse_hosts(Some(" ntfy.example.com, Push.Example.org:8443 ,")).unwrap(),
            vec!["ntfy.example.com".to_string(), "push.example.org:8443".to_string()]
        );
        assert!(parse_hosts(Some("https://ntfy.example.com/")).is_err());
    }

    #[test]
    fn notices_parse() {
        let c = Uuid::new_v4();
        let p = Uuid::new_v4();
        let mut pending = Pending::default();
        pending.add(&c.to_string());
        pending.add(&format!("{c}/{p}"));
        pending.add("not a uuid");
        pending.add(&c.to_string());
        assert_eq!(pending.collections, HashSet::from([c]));
        assert_eq!(pending.personal, HashSet::from([(c, p)]));
    }
}
